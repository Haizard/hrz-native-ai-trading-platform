# Light Pine Evolution — Engineering Roadmap

- **Status:** Phases 0–3, 6 and 7 complete. **Phase 5's boundary is landed
  (authorization token, boundary tests, [`docs/60`](./60-STRATEGY-BOT-BOUNDARY.md));
  its golden strategy/live parity items remain open.** **Phase 4's contract is
  audited and documented ([`docs/61`](./61-CHARTING-CONTRACT.md)); its visual
  acceptance remains open.** The audit, host-data contract, runtime baseline,
  isolation boundary, and evidence-based language architecture decision are
  recorded. No dependency change is approved by that decision.
- **Reconciled:** 2026-11-18 against `docs/23`, `docs/57`, `docs/24` and the
  `pine-lite` tree. The work still outstanding is listed under "Immediate next
  actions"; the per-phase rows in `docs/23` remain the detailed status of
  record.
- **Scope owner:** QUANTERA platform
- **Implementation rule:** ship one reviewable, tested capability per phase

## Mission

Evolve the existing Pine-Lite implementation into a reliable, Rust-native,
AI-authorable quantitative scripting language for indicators and strategies.
Preserve the current language identity and useful behavior. Use the local
reference repositories to compare engineering approaches; do not replace
working platform components or copy third-party code wholesale.

The supplied `pine light new changes.md` is the requirements source for this
roadmap. Its master assignment explicitly requires an implementation audit and
evidence-based architecture decisions before a rewrite or major dependency
choice.

## Non-negotiable constraints

- Keep Light Pine as QUANTERA's user-facing trading language and Rust as its
  implementation/runtime foundation.
- Keep indicator calculations, strategy decisions, risk validation, and broker
  execution as distinct trust boundaries. Indicator scripts cannot submit
  orders.
- Preserve existing script behavior unless a deliberate, documented language
  version change is approved.
- Maintain the `pine-lite` crate's leaf/wasm build constraints; add dependencies
  only when the comparison demonstrates a concrete benefit.
- Reuse the existing chart engine, market-data providers, backtester, and risk
  controls unless an audited deficiency justifies changing them.
- Treat availability as a contract: a feature is supported only when its
  implementation, diagnostics, tests, documentation, and integration agree.
- Learn from reference implementations; any dependency adoption or code port
  requires source review, license review, compatibility analysis, and tests.
- No performance claim without a reproducible baseline and comparable workload.

## Current-state snapshot

### Existing language path

The core implementation is `crates/pine-lite`:

1. `lex.rs` tokenizes source and header options.
2. `parse.rs` builds a span-carrying AST.
3. `typecheck.rs` checks names, arity, and current value types.
4. `limits.rs` enforces static budgets.
5. `interp.rs` executes the AST per bar and returns plots, alerts, and drawing
   objects.
6. `ta.rs` exposes indicator functions, with `analytics-core` as the only
   internal dependency.

The language is interpreted per bar today; a typed IR, optimizer, and
compile-once incremental execution path are design candidates, not existing
capabilities. `sim.rs` contains script-level simulation behavior. Chart
positioning and rendering are integrated through `frontend/chart-engine` and
the frontend shell. Gateway vetting, preview, sandbox, workspace, and AI flows
must be traced end-to-end before making integration claims.

### Verified source-to-execution flow (initial trace)

```text
Indicator Workspace request
  -> api-gateway/indicator_workspace_routes.rs
  -> pine_codegen.rs: prompt, draft, extract, vet/repair (up to 5 attempts)
  -> indicator_preview.rs: backfill/load candles, vet, replay preview
  -> db: persist validated source revision + preview evidence

Attached chart layer
  -> frontend/chart-engine: vet source, run Pine-Lite on visible candles
  -> ScriptPane or ScriptOverlay output
  -> chart scene + frontend renderer

Script strategy
  -> backtester::script_strategy: vet, run source, lower intents to replay signals
  -> existing replay/backtest and risk/execution boundaries
```

The browser chart engine reuses the Pine-Lite crate, which is why wasm32
compatibility matters. The current `sandbox-guest` manifest instead links
`strategy-runtime` and `strategy-dsl`, not `pine-lite`; Pine-Lite's gateway
preview and chart execution are therefore separate paths from the existing
strategy WASM sandbox. Phase 0 must determine the intended isolation boundary
for model-generated Pine-Lite scripts and close any unsafe gap before exposing
new execution capabilities.

The generator's authoring prompt and reference examples live in
`crates/api-gateway/src/pine_codegen.rs`, while actual acceptance is enforced
by `pine_lite::vet`. This is a known capability-drift risk: a future
machine-readable API should derive from or be checked against the actual
runtime/typechecker rather than become a second manually maintained function
inventory.

The initial prompt audit found one concrete mismatch: the shared generation
and repair guide described `pyramiding > 0`, although `vet` rejects it and the
simulator supports one position at a time. The guide now states the enforced
limit, with a regression test preventing the unsupported example from returning.

### Implemented capability is broader than a starter language

The existing spec and tests already describe or cover series/history semantics,
`na`, persistent variables, control flow, arrays, user functions, technical
analysis functions, multi-symbol/timeframe reads, drawing objects, tables,
strategy intents/simulation, and chart rendering. The exact support matrix
must be generated from code and integration tests rather than inferred from
the roadmap in `docs/23`.

The browser UI exposes layer visibility/removal and a settings popover for
boolean and numeric inputs; string/color inputs are currently code-only. The
chart engine vets and interprets source as it builds scenes rather than using
a compile-once cache. An initial representative baseline is recorded in
`docs/58-LIGHT-PINE-PERFORMANCE-BASELINE.md`; comparable follow-up measurements
are still needed before deciding whether caching or an IR is justified.

### Audit flags (status as of the 2026-11-18 reconciliation)

Resolved:

- `docs/23`'s phase/status summaries were reconciled against source and
  executable tests; its phase table is now the detailed status of record.
- The AI-loop wording and the isolation boundary were both settled: server
  preview runs `pine-lite` in a restricted worker at the edge (in-process
  outside Linux), and server-side execution stays preview-only. See
  `docs/23` Phase 8 and `docs/08`.
- The stale Phase 9/10 labels were corrected; the workspace route and
  chart-engine integration are now described as implemented/partial rather
  than stale.
- The `analytics-core` consolidation landed with Phase 3: CCI, MFI, DMI,
  Keltner, Bollinger and A/D now share core formulas, and every advertised
  `ta.*` builtin has metadata and numeric coverage.
- Diagnostics gained stable phase codes, spans, and machine-readable repair
  context (Phase 2).
- The reference-project comparison is complete in `docs/56`.

Still open:

- WASM/native parity for any future intrabar feature (`varip`) — explicitly
  excluded from the equivalence guarantee until pinned by tests.
- MTF history/time semantics and final-bucket reveal, and string locals/`na`
  inside functions — both carried as partial rows in `docs/23` (Phases 11 and
  10).
- Pine-Lite's own guest isolation tests remain separate from the strategy
  sandbox's adversarial suite; the Linux worker probe has not been recorded as
  passing on the deployment kernel.
- `rust indicator language/pine light new changes.md` remains the requirements
  source; no separate `.readme` was supplied.

## Baseline evidence

Command: `cargo test -p pine-lite`

The first baseline run passed all 84 library tests, then exposed two failures
in `tests/drawing_objects.rs`:

- `fib.new` appended directly to `Output.objects`, but interpreter finalization
  replaces that vector from the live object arena. Fibonacci objects were
  therefore lost, and bypassed the shared heap cap.
- The strategy header parser accepted `pyramiding=2`, but the current
  single-position execution model requires rejecting values above zero.

Both were pre-existing implementation defects and were corrected during this
initial audit: `fib.new` now uses the same capped object arena as other drawing
constructors, and vetting rejects unsupported positive pyramiding with an
actionable error.

After those fixes, all Pine-Lite unit, integration, and doc tests passed:
**139 passed, 0 failed** across the crate's test groups. Existing compiler
warnings remain in `analytics-core` and `pine-lite`; they were not broadened
into unrelated cleanup.

The subsequent calculation-ownership audit found `ta.vwap` calling the
whole-window `calculate_vwap` and broadcasting its final result to every bar.
That exposed future candles to historical script evaluation. It now delegates
to `analytics-core::vwap::calculate_vwap_series(..., true)`, which accumulates
causally and resets on the UTC-day boundary; a Pine-Lite regression test pins
both properties. The AI reference now documents that reset behavior too.
Current total: **140 passed, 0 failed** across the Pine-Lite crate test groups.
`cargo test -p chart-engine` also passes (**342 tests**). Both
`pine-lite` and `chart-engine` pass `cargo check` for
`wasm32-unknown-unknown`.

This is a crate-level baseline, not a claim that the full language or platform
integration has been audited.

## Reference repository strategy

The local references are isolated under the root-level ignored folder
`rust indicator language/`. Study them for focused questions:

| Reference | Initial investigation focus | Decision it may inform |
| --- | --- | --- |
| Chumsky | Parser composition, recovery, diagnostics, grammar maintenance | Keep the handwritten parser or adopt a parser library |
| Rhai | Embedded scripting model, host API, limits, compilation | Whether any hybrid embedding is worthwhile |
| Rune | VM, bytecode, modules, host integration | Runtime/IR design alternatives |
| Wasmtime | WASM isolation, fuel, resource controls | Sandbox integration and resource policy |
| YATA | Stateful indicator APIs, formula coverage, licensing | Quant API design and numerical parity |

The source-level comparison of all requested projects is recorded in
`docs/56-OPEN-SOURCE-REFERENCE-BENCHMARK.md`. It pins inspected source
revisions, links relevant implementation/license files, distinguishes
published benchmark evidence from inference, and records integration and
MSRV constraints. TradeRyte Trading Charts and the intended `ta-rs` repository
remain explicitly unresolved rather than guessed. The report recommends no
immediate dependency or runtime replacement; its implementation priorities
remain subject to Phase 0 baselines and review.

## Phased execution plan

### Phase 0 — Repository audit and reproducible baseline

**Work**

- Trace source-to-chart execution across lexer/parser/type checker/runtime,
  gateway vet and preview, sandbox, backtester, chart engine, frontend, and
  AI generation.
- Build a feature matrix with statuses `implemented`, `partial`, `missing`,
  or `data-gated`, each backed by a source path and test. **Done:** see the
  source/test evidence in `docs/23` and the dependency map and capability
  assessment in `docs/57`.
- Resolve the two baseline Fibonacci drawing test failures and run all
  Pine-Lite integration suites. **Done:** both Fibonacci failures were
  repaired; the full crate suite now passes. The previously hidden strategy
  header conformance test also exposed and fixed positive-pyramiding acceptance.
- Record representative AI-generated valid and invalid scripts and
  diagnostics. **Done with a negative acceptance:** deterministic valid and
  runtime-failing preview fixtures are covered by
  `crates/api-gateway/tests/indicator_preview_flow.rs`; the earlier
  provider-backed FVG attempt and its missing-drawings result are recorded in
  `docs/57`, not reported as a pass.
- **Benchmark baseline done:** the dependency-free
  `cargo bench -p pine-lite --bench runtime_baseline` harness measures vetting,
  vetted execution, allocations, and replay after a last-bar update for five
  representative workloads. See
  `docs/58-LIGHT-PINE-PERFORMANCE-BASELINE.md`; this does not establish a
  before/after speed comparison.
- Specify host-provided series, alignment/carry-forward, and missing-data
  behavior. **Host-data contract done:** documented in `docs/23`, with a
  regression test for sparse `sec=` gaps. MTF rolling-history/time semantics
  and the partial-final-bucket behavior remain open.
- **Isolation boundary decided:** the gateway's current native preview stays
  preview-only; do not broaden server-side execution or add trading
  capabilities until Pine-Lite runs as a bounded Wasmtime guest. Wasmtime's
  strategy guest tests are evidence for that sandbox's controls, not evidence
  that Pine-Lite is already isolated. See `docs/08` and `docs/57`.

**Exit gate**

- Dependency/data-flow diagram; authoritative capability matrix; green
  Pine-Lite test suite or explicitly isolated, understood blockers; repeatable
  correctness and performance baselines.

**Exit:** Phase 0's audit and baseline gates are met. The provider-backed FVG
attempt failed its visual acceptance criteria and is an understood blocker,
not a successful AI-generation result. The remaining MTF and Pine-Lite guest
isolation gaps are carried into the next phases with explicit tests and limits.

### Phase 1 — Language contract and reference comparison

**Work**

- Reconcile `docs/23` with the implementation and define versioned execution
  semantics for historical bars, closed-bar updates, `na`, state, timeframe
  alignment, and strategy calls. **Status review done:** `docs/23` now records
  per-phase source/test evidence and marks MTF timestamp/history semantics as
  partial rather than complete.
- Compare the current parser/type checker/interpreter with Chumsky, Rhai, and
  Rune; compare sandbox constraints with Wasmtime.
- Define a quantitative-library comparison using YATA plus selected
  candidates from the requirements list.
- Decide explicitly whether the current frontend stays, receives targeted
  refactors, or gains an optional typed IR. No runtime replacement in this
  phase.

**Exit gate**

- [Architecture decision record](./59-LIGHT-PINE-ARCHITECTURE-DECISION.md)
  reviewed against source, benchmark, and reference research; it records
  migration risks, dependency/license findings, and the intentional no-change
  option. **Exit: Phase 1 complete.**

### Phase 2 — Correctness, types, and diagnostics

**Work**

- Fix proven correctness defects before broadening syntax.
- **Done:** pass raw timestamped `request.security` source candles through
  both hosts; merge by timestamp in the VM so higher-timeframe rolling cadence
  and viewport warm-up use full source history. Runtime tests cover empty,
  missing, sparse, and out-of-order pools, gaps/lookahead, source-close
  boundaries, and partial-final-bucket reveal; chart-engine tests cover
  zoomed-window warm-up. Broader chart-preview/renderer acceptance remains a
  separate visual integration item.
- **Done for the implemented value model:** enforce compatible ternary
  branches (`na` inherits the other branch), declared and reassigned variable
  kinds, input defaults, boolean control-flow conditions, numeric loop bounds,
  typed user-function arguments, UDT constructor fields, and supported
  rendering series. Builtin argument names and kinds are enforced where the
  registry marks metadata complete; incomplete entries remain explicitly
  outside that guarantee.
- **Done:** diagnostics expose source spans, a stable phase code, actionable
  messages, and machine-readable repair context.
- **Done:** negative tests cover invalid operations, missing data, lookahead,
  boundary conditions, resource exhaustion, and the MTF failure cases above.

**Exit gate**

- The declared contracts for the implemented language subset are enforced by
  vetting and conformance tests; existing valid scripts remain
  behavior-compatible. This does not claim complete Pine or complete
  parameter metadata for every builtin. **Exit: Phase 2 complete.**

### Phase 3 — Quantitative standard library

**Work**

- Audit and centralize calculation ownership, reusing `analytics-core` where
  appropriate rather than maintaining divergent formulas.
- Prioritize gaps using real AI prompts and data availability: OHLCV-only,
  trade-level, and order-book-dependent capabilities must be distinguished.
- Add functions in small families with signatures, warm-up/`na` behavior,
  parameter bounds, multi-output behavior, and numerical fixtures.
- **Complete — quantitative standard library:** every advertised `ta.*`
  builtin has complete signatures/defaults, host-data and runtime metadata,
  numeric coverage, and AI-reference discovery. The audit corrected full
  lookback warm-up behavior and `ta.rising`/`ta.falling` semantics, added
  `ta.cmf(length)` and `ta.ao()`, and consolidated compatible CCI, MFI, DMI,
  Keltner, Bollinger, and A/D calculations with `analytics-core`. MACD retains
  its Pine-specific earlier line warm-up because the shared point API only
  emits after its signal line is ready. Unsupported order-flow inputs remain
  unadvertised because the host does not supply trade/footprint data.

**Exit gate**

- **Exit: Phase 3 complete.** Every advertised function is executable,
  documented, numerically tested, and categorized by required input data; no
  placeholder or unsupported API appears in AI capability discovery.

### Phase 4 — Charting contract

**Status: contract audited and documented (2026-11-18); visual acceptance
open.** [`docs/61`](./61-CHARTING-CONTRACT.md) records the end-to-end pipeline,
identity, anchors, pane ownership, lifecycle, caps, serialization, the
per-output coverage matrix, and the proven/open lists. One contract bug was
found and fixed: per-kind caps recycle the oldest drawing's arena slot, so
serialization now orders by creation sequence rather than slot index (later
draws still paint on top), pinned by
`per_kind_caps_rotate_the_oldest_and_keep_creation_order`; box/label deletion
also gained VM and chart-engine mapping tests. Duplicate titles are now
disambiguated (`script:X~2`) instead of colliding, pinned by
`two_scripts_with_the_same_title_get_distinct_ids`. This phase still owns the
visual-acceptance work that
`docs/23` carries as partial on Phases 10, 11 and 16, plus `docs/24`'s open S2
notes. Those rows stay partial until this phase's exit gate is met — a
pine-lite unit test or a projection test alone is not visual acceptance.

**Work**

- Audit existing plot/object contracts and their end-to-end behavior through
  the engine and frontend. — **done** (`docs/61` §1–§7).
- Define stable IDs, coordinates/anchors, pane ownership, object lifecycle,
  limits, serialization, and update semantics. — **done**; duplicate titles no
  longer collide (`script:X~2`).
- Extend the existing renderer contract only for specific missing capabilities
  (for example profiles, heatmaps, or specialized panels), not as a renderer
  rewrite. — no extension added: nothing in the audited requests needs one.

**Exit gate**

- Each supported visual output has language, wire, chart-engine, and visual
  integration tests, including deletion/update/cap behavior.

### Phase 5 — Strategy and bot boundary

**Status: boundary landed (2026-11-18); parity items open.** The trace,
semantics, and authorization model are recorded in
[`docs/60`](./60-STRATEGY-BOT-BOUNDARY.md). Landed: `LiveAuthorization` is the
only way to construct a `LiveBot` (`LiveGate::authorize` mints it; a venue
mismatch is refused at construction), and `crates/backtester/tests/
script_boundary.rs` proves scripts have no broker vocabulary, cannot carry
`qty`, cannot change the risk figure, and can return only a `Signal`.
`trading-engine` still contains no `pine` reference, so a script reaches a
backtest but not a bot. Open: a script→promote→backtest golden, carrying or
refusing script-declared targets, and historical/live parity for a
script-backed strategy (needs a venue/provider harness).

**Work**

- Trace current strategy declarations, intents, deterministic replay, risk
  validation, and broker authorization. — **done** (`docs/60` §2–§7).
- Specify strategy lifecycle, order/position semantics, sizing, stops/targets,
  and historical/live parity. — **done** for semantics; parity open.
- Preserve the risk engine as the sole authority for execution; do not expose
  broker calls to indicator scripts. — **done** and tested.

**Exit gate**

- Golden backtest fixtures, historical/live semantic checks, and tests proving
  scripts cannot bypass risk or execution authorization.

### Phase 6 — Compile-once execution and performance

**Status: Complete (2026-11-12).** A measured repeated whole-series TA
calculation was optimized with a per-VM, call-site result cache for pure
top-level SMA, EMA, RMA, WMA, RSI, and ATR calls. The cache is capped at
1,000,000 result values (8,000,000 bytes of value payload) and 500 call sites;
dynamic or state-dependent expressions retain the interpreter path. Identical
benchmark workloads were run before and after, and output parity, prefix
invariance, and the full Pine-Lite unit suite passed. See the [performance
report](./58-LIGHT-PINE-PERFORMANCE-BASELINE.md).

**Work**

- Measure interpreter costs and allocations on representative indicators,
  multi-timeframe scripts, drawings, and long histories.
- Prototype typed IR or another optimization only where a measured bottleneck
  justifies it.
- Evaluate caching, incremental series updates, and parallelism separately;
  preserve deterministic output and bounded memory/fuel.

**Exit gate**

- **Exit: Phase 6 complete.** Before/after measurements show a material
  improvement for TA-heavy and strategy workloads with no output or prefix
  regressions, and the added cache has an explicit per-VM memory bound.
  Incremental TA state and parallel execution remain deferred: the former
  needs explicit append/amend semantics and the latter has no measured
  workload evidence to justify its determinism and coordination costs.

### Phase 7 — AI Developer Kit and quality gates

**Status: Complete.** `GET /scripts/capabilities` exposes the lexer language
version and compiler-owned builtin catalog; provider/data capabilities remain
available from `GET /capabilities`. The prompt and repair cheatsheet consume
the same builtin registry, diagnostics have stable codes and spans, and the
generation path vets, previews, repairs runtime failures, and persists only a
successful preview. The prompt's reference script now has a candle-fixture
execution test, and the language guide documents version and migration policy.

**Work**

- Generate authoritative machine-readable syntax/API/type/data/drawing
  capabilities from the implementation where possible.
- Add verified examples, diagnostics guidance, script validation, test/preview
  feedback, and capability discovery to the AI workflow.
- Add version compatibility, migration notes, and conformance fixtures.

**Exit gate**

- **Exit: Phase 7 complete.** The compiler registry is discoverable without a
  duplicated API description, generated examples are vetted and executed,
  repair errors are actionable, and runtime preview gates persistence.

## Cross-phase acceptance checklist

- Tests and docs accompany every capability; no unrelated broad changes.
- Preserve wasm32 compatibility and crate dependency rules.
- Run targeted crate tests, relevant chart/gateway integration tests, and
  workspace checks as each phase touches its boundary.
- Check historical/live parity and no-lookahead behavior where market data is
  involved.
- Benchmark only after correctness passes and compare the same script,
  candles, configuration, target, and build mode.
- Audit licenses/notices before adding dependencies or deriving code.
- Report completed work, exact validation results, limitations, and the next
  bounded milestone; do not claim a phase complete based on documentation
  alone.

## Immediate next actions

1. **Phase 4 — charting contract.** The contract and audit are in `docs/61`.
   Remaining: provider-backed visual acceptance for Phases 10/11/16 and the
   `prompt1`/`prompt2` fixtures, the duplicate-title identity decision, and
   shell-level update/delete integration tests.
2. **Phase 5 — strategy/bot boundary.** The boundary, token authorization and
   "a script cannot bypass risk" tests are landed (`docs/60`). Remaining:
   golden script→promote→backtest fixture, script-declared target handling, and
   replay-vs-live parity on the same candles.
3. **Provider-backed generation acceptance.** Still failed, for provider and
   model reasons now recorded in `docs/57`: the full prompt2 transcript exceeds
   every configured provider's context, the distilled fixture against a
   mismatched symbol failed pooled-data prefetch after one repair, and against
   a matching symbol the model never produced a vetting script in five
   attempts. The local gateway is now wired to Postgres 18 on `127.0.0.1:5432`
   (`.env` points there; the previous remote DSN is saved in the ignored
   `.env.remote`), so attempts can be repeated without redeployment. A
   scripted model proves the contract, not a live model's ability to infer the
   strategy from the transcript.
4. **Close the named partials in `docs/23`** that are not visual acceptance:
   MTF sub-VM history/rolling semantics and final-bucket reveal (Phase 11), and
   persistent string locals plus complete string-`na` propagation (Phase 10).
5. **Linux isolation verification.** Phase 8's worker probe is shipped and in CI
   but has not been recorded as passing on the deployment kernel.
