# Light Pine Architecture Decision

- **Status:** Accepted working direction for the Light Pine roadmap
- **Decision date:** 12 November 2026
- **Scope:** Language front end, execution model, quantitative library, and
  their compatibility with QUANTERA's Rust/WASM boundaries
- **Evidence:** [language contract](./23-PINE-LITE-SCRIPTING-LANGUAGE.md),
  [open-source reference research and benchmark](./56-OPEN-SOURCE-REFERENCE-BENCHMARK.md),
  [architecture audit](./57-LIGHT-PINE-ARCHITECTURE-AUDIT.md), and
  [sandbox boundary](./08-SANDBOX-WASM.md)

## Decision

Keep the custom Rust Light Pine lexer, parser, span-carrying AST, type checker,
limits, and per-bar interpreter as the authoritative language implementation.
Make correctness and maintainability improvements in that implementation;
do not replace it with Chumsky, Rhai, Rune, another scripting runtime, or a
native JIT.

Do not add a typed IR or compile-once execution plan yet. The existing
performance harness is a starting point, not evidence that an IR is needed.
Profile representative QUANTERA scripts first. If a repeatable bottleneck is
confirmed, prototype a private typed execution plan for that measured hot path,
retain the interpreter as the reference/fallback, and require behavioral and
performance parity before expanding the prototype.

Keep `analytics-core` as the canonical quantitative implementation. Use
external projects as formula and API-design references; add no indicator or
array dependency in this phase.

This decision does not change the separate server-execution boundary:
gateway preview remains preview-only and in-process until Pine-Lite executes
inside a bounded Wasmtime guest under the gates recorded in
[`docs/08`](./08-SANDBOX-WASM.md).

## Evidence and alternatives

### Front end and language runtime

The current pipeline is a custom lexer, newline/indentation-aware parser,
span-carrying AST, type checker, static limits, and interpreter. It is used by
the native gateway/backtest paths and the browser's WASM chart build. Replacing
it would affect Light Pine syntax, Pine-specific series and `na` behavior,
diagnostics, resource limits, and multiple integrations at once.

- **Keep the handwritten front end (selected).** It avoids a second grammar
  source and protects existing syntax/semantics. Improve diagnostics,
  conformance fixtures, builtin metadata, and correctness at the existing
  boundaries.
- **Adopt Chumsky (deferred).** Its composable parsing and recovery are useful
  design references, but no Light Pine recovery or maintenance benchmark
  demonstrates enough benefit to justify migration. A future experiment must
  preserve indentation/newline rules and prove error-recovery gains against
  the current parser.
- **Replace Light Pine with Rhai or Rune (rejected).** Both are complete
  embedded languages, not drop-in parser or VM components. A replacement
  would duplicate or discard Pine-specific semantics and move existing
  integrations to a different language; it does not establish a performance
  win for QUANTERA workloads.
- **Add a typed IR/compile-once plan now (deferred).** The source audit
  identifies repeated whole-series TA work as a hypothesis, not a measured
  result. First record cold vet and warm execution, latency percentiles,
  allocations/peak memory, interpreter steps, and output equality across the
  representative workloads in `docs/56`. A typed plan is justified only by a
  repeatable improvement on the identified hot path without changing outputs,
  prefix invariance, no-lookahead behavior, or native/WASM parity.
- **Add a native JIT (rejected for this roadmap).** Cranelift would add another
  backend before a verified typed plan exists and complicate browser/WASM
  support. QUANTERA already uses Cranelift under Wasmtime; that is not evidence
  for a Light Pine JIT.

### Quantitative library

Keep formulas in `analytics-core` where shared ownership is appropriate, and
keep Pine-specific warm-up, session, and `na` behavior explicit at the
`pine-lite` boundary. The source comparison found useful reference patterns:
YATA separates indicator configuration from stateful instances and supports
multi-value outputs, but its RSI scale and warm-up rules need parity review;
Wickra demonstrates update/reset and batch APIs but requires Rust 1.86, above
the workspace's Rust 1.85 floor at the inspected version. TA-Lib is a useful
differential reference, not a direct dependency recommendation.

No candidate currently establishes a concrete feature or performance need
that outweighs dependency, portability, semantic-parity, and license-review
costs. Reconsider a dependency only for a named capability with a demonstrated
gap, matching numerical fixtures, supported build targets, and a reviewed
version, MSRV, dependency tree, and license obligations.

### Isolation

The existing Wasmtime strategy guest does not link Pine-Lite; gateway preview
calls the native interpreter. Continue to treat preview as preview-only.
Moving Pine-Lite server execution into Wasmtime is a separate, bounded
isolation project, not a reason to replace the language runtime. The guest
must preserve fuel, epoch, memory, import, and error controls and pass
native/WASM equivalence and adversarial tests before broader execution.

## Dependency, compatibility, and migration risks

- **Dependencies:** This decision adds none. The inspected references have
  differing licenses and MSRVs; several exceed Rust 1.85. The research's
  license matrix is preliminary, not a transitive license audit. Re-check
  exact versions, features, transitive licenses, notices, and target support
  before any adoption.
- **Language compatibility:** Parser or runtime replacement risks silently
  changing syntax, series alignment, state, warm-up, `na`, timeframe, and
  strategy semantics. Keep `docs/23` and executable fixtures authoritative;
  introduce deliberate behavior changes only through an explicit language
  version decision.
- **Integration:** Gateway preview, backtesting, and browser WASM use distinct
  paths. Any internal execution-plan experiment must test the actual consuming
  paths rather than only a microbenchmark.
- **Optimization:** Caching or precomputation can introduce stale state or
  lookahead. Cache only proven-pure computations over immutable inputs;
  define amend-last-bar and reset behavior for mutable streaming state.
- **Isolation:** A Wasmtime guest is an in-process WebAssembly boundary, not
  OS-process isolation. Do not treat this language-architecture decision as
  approval to broaden server-side execution.

## Revisit criteria

Reopen this decision only when at least one of these has evidence:

1. Reproducible profiling shows a material Light Pine workload bottleneck and
   an isolated typed-plan prototype improves it while passing semantic,
   resource, and cross-target parity gates.
2. Parser maintenance or recovery failures are measured against representative
   malformed scripts and a bounded Chumsky prototype materially improves
   diagnostics without creating a second semantic authority.
3. A specific quantitative capability cannot be delivered correctly or
   efficiently in `analytics-core`, and a candidate passes numerical,
   portability, MSRV, dependency, and license review.
4. Product requirements call for server-side Pine execution; the separate
   Wasmtime guest release gates must then be completed first.

Until a revisit criterion is met, the no-change option is intentional:
preserve the custom interpreter, add no compiler/runtime or quantitative
dependency, and spend the next implementation phase on proven correctness,
typed contracts, and diagnostics.
