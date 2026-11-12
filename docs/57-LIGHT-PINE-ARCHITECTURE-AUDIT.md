# Light Pine architecture audit

## Scope and conclusion

This is a source-based audit of the existing QUANTERA implementation, not a
rewrite proposal. It traces the compiler/runtime, AI generation, script storage,
chart rendering, backtesting, and live strategy path. The current custom
Rust-native language is substantial: its lexer, parser, checker, interpreter,
indicator functions, resource limits, chart output, and AI repair path already
work together. The evidence does not justify replacing Light Pine with Rhai,
Rune, another language, or a new runtime.

The main engineering gap is not missing syntax alone. It is the absence of one
authoritative language/API contract and reusable compiled artifact, plus
incomplete parity between preview, backtesting, and production bot execution.
The smallest safe direction is to preserve the interpreter and incrementally
improve its specification, diagnostics, execution parity, and measured
performance.

## Source-to-execution dependency map

```text
Light Pine source
  └─ pine-lite::vet
       ├─ handwritten lexer: header + tokens
       ├─ handwritten indentation-aware parser: span-carrying AST
       ├─ static type/name/arity checker
       └─ static limits checker
            └─ parsed AST + Header
                 ├─ pine-lite::run: per-bar AST interpreter
                 │    └─ Output: plots, shapes, drawings, intents,
                 │       strategy simulation, and other script results
                 ├─ api-gateway preview: host data + Inputs → Output
                 │    └─ immutable indicator revision + preview JSON
                 ├─ chart-engine (Rust/WASM): vet + run + scene projection
                 │    └─ panes, overlays, drawings, strategy layer/equity
                 └─ backtester::ScriptStrategy: rolling candles → signals
                      └─ existing deterministic replay

Production bots: strategy DSL / concept composition → trading engine →
Wasmtime-backed strategy sandbox. The audited Pine-Lite backtest adapter is
not the production bot path.
```

### Key implementation references

- Language pipeline and public vet entry point: `crates/pine-lite/src/lib.rs`.
- Lexer, AST/parser, checker, limits, and interpreter:
  `crates/pine-lite/src/{lex.rs,parse.rs,typecheck.rs,limits.rs,interp.rs,ta.rs}`.
- AI prompt, reference, tool contract, and vet-repair loop:
  `crates/api-gateway/src/pine_codegen.rs`.
- Iterative workspace editing, preview, revision creation, and strategy
  promotion: `crates/api-gateway/src/indicator_workspace_routes.rs`.
- Preview market-data preparation: `crates/api-gateway/src/indicator_preview.rs`.
- Immutable revision persistence:
  `crates/db/src/indicator_workspaces.rs` and
  `crates/db/migrations/0005_indicator_workspaces.sql`.
- WASM chart execution and projection:
  `frontend/chart-engine/src/script.rs`.
- Pine backtest adapter: `crates/backtester/src/script_strategy.rs`.
- Production decision and isolation boundary:
  `crates/trading-engine/src/decisions.rs` and `crates/sandbox`.
- Existing language contract and strategy simulation notes:
  `docs/23-PINE-LITE-SCRIPTING-LANGUAGE.md` and
  `docs/24-STRATEGY-EXECUTION.md`.

## Current capability assessment

| Area | Source-observed state | Important boundary |
| --- | --- | --- |
| Lexing and parsing | Handwritten lexer and indentation-aware parser; parser builds span-carrying AST and collects errors. | No generated grammar or incremental parser. |
| Type checking | Static name, call, arity, and assignment checks; `int`/`float` unify. | Core `Ty` is `Float`, `Bool`, `String`, `Color`, `Na`; it is not a general series-aware or nominal type system. |
| Declarations and control flow | Variables, functions/methods, user-defined type declarations, `if`, `for`, `while`, `switch`, arrays, maps, and matrices are represented/handled in the language. | No enum declaration or import/module system was found. Type and collection behavior is not equivalent to a full Rust-like type system. |
| Execution | Deterministic per-bar AST interpreter with history buffers, `var` state, series caches for eligible calls, and pooled security-expression caching. | There is no typed IR or compile-once bytecode/native backend; some variable-dependent expressions are recalculated. |
| Resource limits | Static statement/plot/fill/function/loop-depth limits; runtime fuel, call-depth, array count/length, object caps; history limit from the header. The strategy Wasmtime sandbox separately enforces fuel, epoch deadlines, guest linear memory, and an import allowlist. | Gateway preview still invokes native `pine_lite::run` in-process. Pine-Lite limits are not memory/OS isolation; the strategy guest tests do not prove a Pine-Lite guest exists. The timeout ticker must start successfully or sandbox construction now fails. |
| Builtins and indicators | Pine-Lite exposes math, TA, input, strategy, data/security, and drawing APIs; several TA formulas wrap or reuse `analytics-core`. A versioned catalog now records arity/result shape for all entries, with parameter, host-data, and runtime metadata explicitly marked complete only for selected TA, data, and drawing calls. | The catalog remains intentionally partial; the interpreter is still the execution authority, and metadata completeness does not prove numerical parity with `analytics-core`. |
| Market data | Host supplies candles, aligned secondary candles, pooled symbol/timeframe series, ticker/data series, and instrument metadata where available. | Raw trades, full order books, and footprint data are not general script inputs merely because related Rust data structures exist elsewhere. |
| Chart output | VM emits structured plot and object output; Rust/WASM chart engine maps it to panes, price overlays, shapes, and drawings. Strategy outputs include chart-side simulation rendering. | Do not imply support for every requested visualization (for example, full order-book heatmaps or native footprint panels) without a script API, renderer mapping, and tests. |
| AI integration | Gateway supplies a language reference and `submit_script` tool; static errors are fed back through a bounded retry loop; preview failures can trigger a runtime-repair pass. Prompt and repair call bounds now render from the versioned builtin catalog. A provider-backed indicator generation created a validated revision, but its preview emitted no drawings. | The live attempt proves generation and persistence, not semantic correctness: the requested FVG visualization was absent. Unspecified registry entries remain explicitly incomplete. |
| Script storage | Workspace revisions store source and validated preview; edits use the active Pine-Lite source as the next model input. | Revision history is intentionally immutable; this is not a compiler cache. |
| Strategy/backtesting | Pine strategy calls can be simulated in the VM/chart; `ScriptStrategy` adapts Pine source to the existing replay. | The replay adapter is not constructed by a production bot path; live bot execution remains Strategy DSL/sandbox based. |
| Diagnostics | Errors carry a kind, line/column span, and message, and the vet path returns multiple errors. | No stable machine-readable diagnostic code, severity, byte range, or related-location contract was found. |

## Architectural findings

### 1. Keep the custom language; do not replace the runtime prematurely

`pine_lite::vet` lexes, parses, checks, and enforces limits before returning an
AST. `pine_lite::run` executes that AST once per bar over host-provided candles.
The Rust chart engine compiles the same crate to WASM. This is a coherent,
Rust-native implementation and already supports trading-specific semantics
that a general embedded scripting runtime would not supply automatically.

Rhai and Rune remain useful references for API design, diagnostics, and runtime
engineering, as described in the open-source reference report. Adopting either
would still require a Light Pine front end and trading semantics; it would not
automatically deliver native execution speed, typed series semantics,
deterministic replay, or chart integration. No runtime replacement is
recommended absent representative benchmarks and a demonstrated capability
defect.

### 2. AI reliability depends on a single source of truth

The AI prompt/reference, checker arity/type rules, interpreter dispatch, and
tests overlap but are not generated from one machine-readable builtin
specification. This creates a direct source of model hallucinations and
repair-loop failures: a function may be described differently from its
accepted signature or runtime behavior. The passed generator tests demonstrate
coverage for selected examples, not exhaustive contract synchronization.

The current workspace flow does address two previously reported UX issues:
existing Pine-Lite source is supplied when editing the active revision, and a
revision is not created until static validation and preview succeed. Keep the
single-script revision workflow; do not create a fresh unrelated script for
each edit.

### 3. Execution paths have different operational guarantees

- Browser/chart execution vets and runs source in the Rust/WASM chart engine.
- Gateway preview uses the same interpreter but executes in the gateway
  process; interpreter fuel and object/array caps are not OS-level isolation.
- The Pine backtest adapter reruns the script over its rolling candle window.
  `Strategy::take_execution_error` now gives adapters a backward-compatible
  error channel; Pine interpreter failures propagate from replay as
  `BacktestError::StrategyExecution` instead of looking like “no signal.” A
  regression test verifies a vetted script's runtime error aborts replay with
  its interpreter diagnostic.
- Production bots use a separate strategy DSL and Wasmtime-backed sandbox.
  A validated indicator revision is not, by itself, a live trading strategy.

These are separate integration choices, not evidence that the underlying
language must be replaced. Before connecting Pine-Lite to live execution,
define one explicit execution contract, surface runtime failures, and preserve
the platform's existing risk and authorization boundary.

### 4. Documentation status has drifted from source

The language document previously described the chart-side strategy simulator
and in-VM account state as open work. `docs/24-STRATEGY-EXECUTION.md` records
S1–S3 as implemented and S4 as pending; the source has VM simulation and chart
projection paths. The old status note in docs/23 is corrected to point to
docs/24 and to distinguish chart simulation from live-bot deployment.

The language phase table in `docs/23` has now been reconciled against source
and executable tests. Phases 1–7 and 12–15/17 are marked complete only for
their scoped deliverables. Phase 8–11 and 16 remain partial for the explicitly
listed isolation, workspace-switch, MTF-history, and timestamp gaps. Do not
infer gateway/browser/backtester parity from a Pine-Lite unit test alone.

## Recommended staged engineering work

1. **Authoritative Light Pine capability registry.** A versioned
   `pine-lite::builtins::CATALOG` centralizes accepted call arities, static
   result kinds, and multi-output shapes; the type checker reads it, and the AI
   generation prompt and repair cheatsheet render their call-bound inventory
   from it. Schema v2 adds typed parameter names/defaults, host-data
   requirements, and runtime-support status for selected TA, request, and
   drawing entries; the prompt explicitly labels all other entries incomplete.
   Coverage is still partial and must not be described as a complete SDK
   specification. Tests pin the metadata completeness boundary, time-vs-bar
   drawing parameters, and documented MTF/drawing calls. This work also
   corrected `request.security`'s optional named-argument bounds and drawing
   constructor arities, in addition to the earlier `ta.dmi` mismatch.
2. **Structured diagnostics and conformance fixtures.** Implemented stable
   phase-level diagnostic codes and structured spans while preserving readable
   messages and legacy line/column fields. A bounded top-down-analysis request,
   distilled from the supplied prompt, now exercises the gateway's generation
   tool-call and vet gate with a deterministic scripted LLM; the resulting
   15m/1h closed-bar FVG script is executed and its time-anchored zones and
   labels are verified in both the gateway conformance test and chart-engine
   projection test. This proves the generation contract and script behavior,
   not a live model's ability to infer the strategy from the full transcript;
   that still requires a successful provider-backed visual run. The workspace response now
   appends a server-authored receipt with the saved active revision and replay
   counts, independent of the model's prose; the gateway integration test
   checks that receipt against the stored revision and active pointer. Preview
   stats now also report structured drawing-object, box, and label counts from
   interpreter output, plus whether the drawing limit was reached; the
   gateway integration test exercises one box and one label. A live run using
   the supplied transcript and explicit output requirements saved revision #1,
   but replay reported one plot and zero drawing objects, boxes, or labels.
   Inspection of its source showed that `request.security` fetched only HTF
   timestamps while FVG conditions used chart-timeframe OHLC; the bullish
   condition also required `low > high[length]` and `close < high[length]`,
   which cannot both hold. Therefore this is not a successful MTF FVG test.
   Provider-backed generation exhaustion now has a distinct indicator error
   instead of being mislabeled as a strategy document failure.
3. **Execution-parity and error handling.** Pine script runtime failures abort
   replay with their interpreter diagnostic. A shared, deterministic drawing
   fixture now verifies preview statistics, chart-engine projection, and
   replay's no-trade behavior for the same source. This closes only the
   single-timeframe drawing smoke-test gap; it does not establish full MTF host
   data or live-execution parity. Keep live execution separate until
   authorization, risk, and data semantics are specified.
4. **Compile-once representation and benchmark baseline.** The dependency-free
   harness now measures representative simple TA, loops/state, drawing-heavy,
   security/MTF, and strategy scripts: vetting, execution, allocations, and
   replay after a last-bar update. Results and limitations are recorded in
   `docs/58-LIGHT-PINE-PERFORMANCE-BASELINE.md`. This is a local baseline, not a
   before/after comparison. Use comparable follow-up runs to establish whether
   a typed IR, caching parsed/checked scripts, or parallelism is warranted;
   include cache invalidation/version semantics before sharing compiled
   artifacts.
5. **Explicit host-data capabilities.** The host-data contract is documented
   in `docs/23-PINE-LITE-SCRIPTING-LANGUAGE.md`: primary candles, `sec=`,
   pooled MTF, lower-timeframe reads, `request.data`, and missing-feed behavior.
   The sparse `sec=` aligner now carries flat OHLC with zero volume across gaps
   and before its first source bar, with a regression test. MTF history/time and
   final-bucket limitations remain open. Add trades/order-book/footprint types
   only alongside real data providers and executable tests; OHLCV-only
   calculations must not be described as order-flow measurements.
6. **Pine-Lite server-side isolation.** The boundary decision is recorded in
   `docs/08-SANDBOX-WASM.md`: keep the existing gateway preview limited to
   preview results and do not expand server-side execution or add trading
   capabilities until
   the trusted Pine-Lite interpreter runs as a bounded Wasmtime guest. Pass
   provider data as capped input; expose no filesystem, network, secret,
   broker, or order-placement capabilities. Wasmtime is an in-process
   WebAssembly boundary, not an OS-process boundary; a separate restricted
   worker is required if the deployment must contain a Wasmtime/host-runtime
   compromise. Existing Pine-Lite fuel tests and strategy-sandbox adversarial
   tests establish only their respective current controls, not Pine-Lite
   sandboxing. The Wasmtime epoch ticker now fails closed if its thread cannot
   start, and the adversarial suite tests epoch interruption while fuel
   remains. A Pine-Lite guest still needs its own input/output, memory, fuel,
   timeout, capability, concurrency, and native-parity tests before expansion.

## Verification performed

- Before the registry change, `cargo test -p pine-lite -p chart-engine
  -p backtester` passed (140 Pine-Lite, 342 chart-engine, 38 backtester tests,
  plus one doctest).
- After the registry change, `cargo test -p pine-lite` passed (144 tests) and
  `cargo test -p api-gateway pine_codegen::tests` passed (38 tests).
- `rustfmt --edition 2021 --check` passed for the changed Rust files. A
  workspace-wide format check was stopped after showing extensive formatting
  differences in unrelated files.
- The workspace emitted existing compiler warnings; no test failures occurred.
- A provider-backed indicator-agent attempt using the supplied `prompt1.text`
  saved a revision, but its preview showed one plot and zero drawing objects,
  boxes, or labels. This failed the requested FVG visual acceptance criteria;
  it is not a successful live-agent test. No follow-up provider-backed run was
  possible in the current environment because the local chat/UI and gateway
  ports were not listening and no local `DATABASE_URL` was configured. The
  deterministic generator gate is not a substitute for this test.
- `cargo test -p api-gateway secondary_candles_carry_flat_close_with_zero_volume_across_gaps`
  passed for the `sec=` alignment regression.
- `cargo test -p pine-lite -p backtester -p chart-engine` passed; the targeted
  gateway library and preview tests passed.
- `cargo test -p sandbox --test adversarial` passed (23 tests), including
  forbidden-import, fuel, memory, trap, and epoch-timeout cases. The
  Pine-Lite `a_runaway_while_dies_on_the_fuel_budget_not_on_a_hang` test also
  passed.
- `cargo bench -p pine-lite --bench runtime_baseline` completed twice; the
  harness and measured medians are documented in
  `docs/58-LIGHT-PINE-PERFORMANCE-BASELINE.md`. Existing workspace warnings
  remain. No before/after comparison or speed claim is made.

The host-data correction changes sparse `sec=` alignment, while the benchmark
adds no runtime dependency and does not change interpreter execution.
