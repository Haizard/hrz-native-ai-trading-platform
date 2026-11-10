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
| Resource limits | Static statement/plot/fill/function/loop-depth limits; runtime fuel, call-depth, array count/length, object caps; history limit from the header. | Fuel bounds interpreter work but is not process isolation. Gateway preview runs in-process. |
| Builtins and indicators | Pine-Lite exposes math, TA, input, strategy, data/security, and drawing APIs; several TA formulas wrap or reuse `analytics-core`. | Type/arity metadata, interpreter dispatch, and AI-facing references are maintained in separate paths; names alone do not ensure numerical parity with `analytics-core`. |
| Market data | Host supplies candles, aligned secondary candles, pooled symbol/timeframe series, ticker/data series, and instrument metadata where available. | Raw trades, full order books, and footprint data are not general script inputs merely because related Rust data structures exist elsewhere. |
| Chart output | VM emits structured plot and object output; Rust/WASM chart engine maps it to panes, price overlays, shapes, and drawings. Strategy outputs include chart-side simulation rendering. | Do not imply support for every requested visualization (for example, full order-book heatmaps or native footprint panels) without a script API, renderer mapping, and tests. |
| AI integration | Gateway supplies a language reference and `submit_script` tool; static errors are fed back through a bounded retry loop; preview failures can trigger a runtime-repair pass. | The model-facing reference is not generated from the same authoritative registry as checker/runtime support. |
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
  Its `Strategy::on_candle` path currently converts interpreter failure to
  `None` (`run_script(...).ok()?`), which can make a failed run look like “no
  signal.” That behavior should be made observable before expanding strategy
  use.
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

The language phase table still contains statuses and dates that warrant a
separate end-to-end reconciliation. In particular, document completion only
when the named acceptance criteria have executable tests in every relevant
host (gateway, browser/WASM, backtester, or live execution as applicable).

## Recommended staged engineering work

1. **Authoritative Light Pine capability registry.** The first slice is now
   implemented: versioned `pine-lite::builtins::CATALOG` centralizes accepted call arities,
   static result kinds, and multi-output shapes; the type checker reads it, and
   the AI generation prompt and repair cheatsheet render their call-bound
   inventory from it. Tests verify unique metadata, selected edge cases, and
   that both AI paths include the generated catalog. This surfaced and fixed
   the `ta.dmi` mismatch: the checker formerly required two arguments although
   its implementation and documented form use one. The registry still needs
   parameter names/defaults, host-data requirements, and runtime support status
   before it is a complete SDK specification.
2. **Structured diagnostics and conformance fixtures.** Implemented stable
   phase-level diagnostic codes and structured spans while preserving readable
   messages and legacy line/column fields. A bounded top-down-analysis request,
   distilled from the supplied prompt, now exercises the gateway's generation
   tool-call and vet gate with a deterministic scripted LLM; the resulting
   15m/1h closed-bar FVG script is executed and its time-anchored zones and
   labels are verified in both the gateway conformance test and chart-engine
   projection test. This proves the generation contract and script behavior,
   not a live model's ability to infer the strategy from the full transcript;
   that still requires a provider-backed run. The workspace response now
   appends a server-authored receipt with the saved active revision and replay
   counts, independent of the model's prose; the gateway integration test
   checks that receipt against the stored revision and active pointer. Preview
   stats now also report structured drawing-object, box, and label counts from
   interpreter output, plus whether the drawing limit was reached; the
   gateway integration test exercises one box and one label.
3. **Execution-parity and error handling.** Make backtest runtime errors
   explicit; test identical source/inputs across preview, chart/WASM, and
   replay where the semantics are intended to match. Keep live execution
   separate until authorization, risk, and data semantics are specified.
4. **Compile-once representation and benchmark baseline.** Add representative
   scripts (simple TA, loops/state, drawing-heavy, security/MTF, strategy) to a
   reproducible benchmark. Measure vet, execution, allocations, and repeated
   chart updates before choosing a typed IR, caching parsed/checked scripts, or
   parallelism. Include cache invalidation/version semantics before sharing
   compiled artifacts.
5. **Explicit host-data capabilities.** Specify which host-provided series
   scripts may read, their alignment/carry-forward semantics, and missing-data
   behavior. Add trades/order-book/footprint types only alongside real data
   providers and executable tests; OHLCV-only calculations must not be
   described as order-flow measurements.
6. **Isolation decision.** Document the threat model for AI-generated scripts.
   If arbitrary scripts are run in a server process beyond tightly bounded
   preview, integrate a dedicated sandbox boundary or otherwise establish and
   test the required resource and access controls. Do not label fuel metering
   as process isolation.

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
- No benchmark directory/harness was found during the audit, so no performance
  comparison or speed claim is made.

This first registry slice changes vet-time metadata and AI reference text; it
does not change the runtime backend or add dependencies.
