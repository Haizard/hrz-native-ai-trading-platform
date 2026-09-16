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

## Tests that fail for the right reason

The suite is the verification gate for every phase, so a test that fails for a reason
other than the one it names is worse than a missing test: it costs a debugging session,
and it can hide a real regression behind a plausible-looking failure. These are rules
this project has had to re-derive, each from a failure that pointed at the wrong line.

- **A test asserts its own setup and teardown.** `load_flow`'s cleanup ignored each
  `DELETE`'s status, so a delete that timed out surfaced *later* as a foreign-key
  violation. `bot_flow` discarded the status of its own create, so a database stall
  arrived as `called \`Option::unwrap()\` on a \`None\` value` at the extraction, naming
  neither the route nor the cause. `Harness::created` and `Harness::ok` exist so the
  failure names the route and prints the body.
- **Never read the clock before the thing you are timing.** Capturing `now` before
  `feed_candle` — which stamps arrival a few hundred nanoseconds later — made an age
  assertion read `599.9999982` instead of `600` and fail intermittently.
- **Never size a fixture at exactly a boundary constant.** The lagged-reader test used a
  literal `4096`, which *is* `DEFAULT_CAPACITY`, so nothing ever lagged and the test
  asserted the opposite of its claim. Size off the constant.
- **Every guard is shown to fail against the bug it names.** Reintroduce the bug
  temporarily, watch the guard fail, revert. A guard nobody has seen fail is a guard
  nobody knows works — this is how the delta-publishing and non-finite-value guards were
  confirmed.
- **Database-backed tests run one at a time per test process.** The managed Postgres
  costs roughly a second per statement over a ten-connection pool, so concurrency
  produces acquire timeouts that read as defects rather than as contention. See the
  `DB_TURN` mutex in `crates/api-gateway/tests/common/mod.rs`.
- **Do not assert a timing that depends on scheduling.** Assert the ordering the code
  guarantees, or the value a deterministic input produces — never "this finished first".

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
