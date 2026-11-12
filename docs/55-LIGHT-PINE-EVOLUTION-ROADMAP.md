# Light Pine Evolution — Engineering Roadmap

- **Status:** Phases 0, 1, and 2 complete. The audit, host-data contract,
  runtime baseline, isolation boundary, and evidence-based language
  architecture decision are recorded. No runtime or dependency change is
  approved by that decision.
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

### Audit flags to resolve

- `docs/23-PINE-LITE-SCRIPTING-LANGUAGE.md` contains multiple phase/status
  summaries that may be dated or inconsistent; reconcile them against the
  current gateway, sandbox, backtester, and frontend before updating the spec.
- The AI-loop description in `docs/23` calls preview execution sandboxed, but
  `indicator_preview::replay_script_preview` currently calls
  `pine_lite::interp::run` in the gateway process. `sandbox-guest` does not
  depend on `pine-lite`; it serves `strategy-runtime`/`strategy-dsl`. Resolve
  this wording and record the intended isolation boundary before adding
  execution capabilities.
- The Phase 9/10 status table in `docs/23` appears stale: the workspace route
  already invokes Pine-Lite generation, vet/repair, preview, and revision
  storage, while `chart-engine` already runs scripts and positions output.
  Complete the UI/integration audit before changing the status labels.
- The documented single-source-of-truth rule for indicator math needs an
  audit against `pine-lite/src/ta.rs` and `analytics-core`. Direct wrappers
  currently cover SMA, EMA, RSI, and ATR; VWAP now uses the core per-bar
  series API. `pine-lite/src/ta.rs` also reimplements formulas with existing
  core counterparts (including MACD, Bollinger, stochastic, CCI, MFI,
  Williams %R, OBV, and Supertrend). Compare warm-up/edge-case behavior before
  consolidating; function names alone do not prove parity.
- Current diagnostics are span-based line/column messages; inspect real model
  repair failures and determine which diagnostic structure or wording changes
  would measurably improve repair success.
- The provided language-evolution folder contains checkouts for Chumsky,
  Rhai, Rune, Wasmtime, and YATA. The broader research prompt names additional
  projects that are not present in this local folder; fetch or inspect those
  separately only when the relevant workstream begins.
- The user confirmed that
  `rust indicator language/pine light new changes.md` is the requirements and
  conversation source to use. No separate `.readme` file was present in the
  supplied folder.

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

**Exit gate**

- Each advertised function is executable, documented, numerically tested, and
  categorized by required input data; no placeholder or unsupported API
  appears in AI capability discovery.

### Phase 4 — Charting contract

**Work**

- Audit existing plot/object contracts and their end-to-end behavior through
  the engine and frontend.
- Define stable IDs, coordinates/anchors, pane ownership, object lifecycle,
  limits, serialization, and update semantics.
- Extend the existing renderer contract only for specific missing capabilities
  (for example profiles, heatmaps, or specialized panels), not as a renderer
  rewrite.

**Exit gate**

- Each supported visual output has language, wire, chart-engine, and visual
  integration tests, including deletion/update/cap behavior.

### Phase 5 — Strategy and bot boundary

**Work**

- Trace current strategy declarations, intents, deterministic replay, risk
  validation, and broker authorization.
- Specify strategy lifecycle, order/position semantics, sizing, stops/targets,
  and historical/live parity.
- Preserve the risk engine as the sole authority for execution; do not expose
  broker calls to indicator scripts.

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

**Work**

- Generate authoritative machine-readable syntax/API/type/data/drawing
  capabilities from the implementation where possible.
- Add verified examples, diagnostics guidance, script validation, test/preview
  feedback, and capability discovery to the AI workflow.
- Add version compatibility, migration notes, and conformance fixtures.

**Exit gate**

- AI-facing capabilities match what runtime executes; generated scripts pass
  vetting, run against fixtures, and receive actionable errors before install.

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

1. Complete Phase 0's cross-crate flow map and feature matrix.
2. Reproduce and resolve the two baseline Fibonacci drawing test failures.
3. Capture representative AI-generated valid and invalid scripts, diagnostics,
   and existing benchmark workloads.
4. Start Phase 1 only after the current implementation and baseline are
   sufficiently mapped.
