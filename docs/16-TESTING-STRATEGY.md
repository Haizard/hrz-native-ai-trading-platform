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
  confirmed. For the browser shell this is a tool rather than a habit:
  `tools/guard_check.py` patches `frontend/app/app.js` one defect at a time, runs
  `tools/shell_check.mjs`, and requires the *named* check to fail, restoring the file on
  every exit path. Doing it by hand is what left the shell mutated after a run was
  interrupted.
- **A test suite that crashes on a broken subject reports nothing, and from a distance a
  crash looks like a pass.** The mutation run above found this the hard way: several
  deliberate defects made the harness exit on a `waitFor` precondition instead of
  printing a failure, so the defect and a green run were indistinguishable in the output.
  Preconditions that are *also* the thing under test must not be fatal — assert them and
  skip what depends on them.
- **A check can pass because something else happened.** Two of the pane checks were
  green for reasons other than the code they named: the harness was dispatching a
  non-bubbling `change` event (no browser sends one, and the page listens on the
  container), and a check counted a series an earlier section had already fetched, so it
  could not fail. Both were found by breaking the thing the check claimed to cover. When
  a check counts something, count the *change*, not the presence. The same trap caught a
  footprint check that counted bare numbers in the paint log: the price-axis tick labels
  (`98.34`, `101.31`) matched the pattern, so the check would have passed with every
  ladder cell blank. It now asserts the contract that actually matters (`grid.show_text`)
  rather than a string that happens to look like one.
  **And it caught one of mine, written the same afternoon as this paragraph.** The check for
  "the numbers in this window need a narrower cell than the constant this replaced" was
  written as "the engine's figure differs from the constant" — and passed against a
  `min_cell_px` that was `undefined`, because `NaN !== 12` is true. The wasm was stale, so
  the field did not exist, and a check about a *number* was satisfied by the absence of one.
  It now asserts `Number.isFinite(learned)` first and prints the raw value when it fails.
  **If a check compares two things, assert that both are the kind of thing that can be
  compared** — a missing field is not a different value, it is a different question.
- **A fixture tidier than production hides production's defects.** The stub for
  `/footprint` first answered with a *tidied* candle list — OHLC only. The real shell maps
  that same array down to OHLC for the price axis **and** hands it back as the request's
  `footprint` field, so the engine refused it with ``missing field `delta` at line 1
  column 32688``. The stub's tidiness was the only thing wrong. Then the second version
  answered with 20 ladders regardless of the requested window, while the shell sizes its
  column count from the viewport — so the engine correctly said the numbers no longer fit,
  and the harness failed against a defect that did not exist. **A stub that ignores the
  request is testing a different program.** Both were fixed by making the fixture match
  the route: full ladders, and a column count derived from `to - from`.
  **The same trap, third instance, in the timestamps.** The footprint fixture built its
  bars as `open_time: i * 300_000_000_000`, which is tidy, deterministic and starts at the
  epoch. The moment a check compared stored data against the live feed it read *thirteen
  thousand hours behind* — a number so far outside any plausible threshold that the
  assertion would have passed against almost anything. The fixture now ends at a real
  recent millisecond, so the lag under test is a plausible one and the check can fail for
  the reason it names. **Fixtures should be shaped like production even where the shape is
  inconvenient**, and a number that is *absurd* rather than merely wrong is the most
  dangerous kind, because every comparison against it succeeds.
- **A claim about time is checked by moving the clock, not by waiting for it.** The live
  badge's whole value is that it stops saying `live` when frames stop arriving — a threshold
  of `bar * 1.5 + 30s`, which is eleven minutes of real time on a 5m chart. Sleeping for it
  is not an option and shortening it would test a different threshold. `tools/shell_check.mjs`
  therefore replaces `window.Date.now` with `realPageNow() + pageClock.offset` *before*
  injecting the page: the page's clock is fake, the harness's is not, so `waitFor`'s
  deadlines still time out on a real hang. Only ever fake the clock of the thing under test.
  The first version of this check scrolled the wheel first to force a redraw, which would
  have passed for the wrong reason — it would have been testing `render`, not the badge's own
  timer — so it now advances the offset and waits for the timer with **no gesture at all**.
  That is also what makes the matching mutation meaningful: `liveTimer = 0` breaks the check,
  which is how the one-second interval was shown to be load-bearing rather than incidental.
  **A badge refreshed only as a side effect of redrawing can never report the case it exists
  for**, because that case is *nothing is arriving and therefore nothing redraws*.
- **A test that calls the only writer of a metric proves nothing about production.** The
  `MD_FEED_AGE` metric had a passing test — which called `feed_candle` directly. Nothing in
  the *live* path ever called it, so `/metrics` served no `market_data_feed_age_seconds`
  while candles were demonstrably flowing, and the `stale_market_data` alert rule read a
  metric nobody wrote. The test was green the whole time. **Ask of every test that
  exercises a writer: does production reach this same call?** The replacement test drives
  the *bus* — the same thing the live path drives — and was shown to fail when the stamp
  is removed.

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
