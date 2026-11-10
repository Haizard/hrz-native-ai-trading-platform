# QUANTERA Open-Source Reference Research and Engineering Benchmark

**Research snapshot:** 10 November 2026
**Scope:** source-level comparison of the named public projects as references
for improving QUANTERA's Rust-native Light Pine language and adjacent
infrastructure. This is an engineering blueprint, not a migration plan.

## Executive conclusion

QUANTERA does **not** need another trading language, indicator engine,
backtester, or chart renderer. The most useful improvements are architectural
patterns that fit the existing boundaries:

1. Keep Light Pine's lexer, parser, type checker, runtime semantics, and name.
   Consider a typed, compile-once execution plan only after a QUANTERA-specific
   benchmark demonstrates the cost.
2. Make one machine-readable builtin/capability registry authoritative for
   validation, AI documentation, output arity, data requirements, and tests.
   Today signatures, runtime dispatch, and AI guidance are maintained in
   separate places.
3. Evolve indicator calculations toward an explicit batch/stream contract,
   with documented warm-up, reset, data requirements, and multi-output
   semantics. Keep `analytics-core` as the canonical implementation.
4. Close the server-side isolation gap before broadening model-generated
   execution. Reuse the existing Wasmtime host and its resource controls if
   Pine-Lite is moved into a trusted Rust/WASM guest; pass scripts as data,
   rather than compiling user source to native code.
5. Preserve QUANTERA's replay, risk, execution, chart-scene, and AI ownership.
   Learn from the references; do not transplant their platforms or copy their
   source.

No new dependency is recommended for immediate adoption. Some current
candidate releases exceed QUANTERA's Rust 1.85 MSRV; others would add
substantial dependency, licensing, or runtime surface without a demonstrated
need. TradeRyte Trading Charts could not be tied to a verifiable public
repository, and “ta-rs” is ambiguous.

## Scope and evidence method

The research covers the requested 29 unique targets. For each verifiable
project, the cited immutable commit is the source snapshot inspected. Source
modules, manifests/license files, and available release or benchmark evidence
were checked; README-only conclusions were not used. “Performance” below
means the execution/data structures visible in source or a project's
workload-specific benchmark. It is **not** a cross-project speed ranking.

The local `rust indicator language/` checkouts contain source but no upstream
`.git` metadata. Their package manifest versions were recorded locally
(Chumsky 0.13.0, Rhai 1.27.0, Rune 0.15.0, Wasmtime 52.0.0-dev, YATA 0.7.0);
they cannot prove an upstream commit. Public findings therefore link to pinned
upstream revisions instead. The directory remains ignored and unchanged.

The license review is a preliminary dependency screen, not legal advice. The
repository manifests declare `MIT OR Apache-2.0`; any product-specific
distribution terms, transitive licenses, notices, and dependency-feature
choices still require the project's normal approval. No reference code or
dependency was added as part of this research.

## QUANTERA baseline and source-level findings

### Existing Light Pine architecture

`crates/pine-lite` is a Rust leaf that depends internally only on
`analytics-core`. Its custom pipeline is:

```text
lex.rs -> parse.rs (span-carrying AST) -> typecheck.rs -> limits.rs
       -> interp.rs (per-bar execution) -> ta.rs -> analytics-core
```

The interpreter stores series/history and state in per-run buffers, uses
static script limits, a runtime step budget, and a call-depth cap. It is
compiled for the browser's WASM chart artifact. This is not a typed-IR or
compile-once VM today.

Useful source anchors:

- [`pine-lite/src/lib.rs`](../crates/pine-lite/src/lib.rs) documents the
  pipeline and `vet`.
- [`lex.rs`](../crates/pine-lite/src/lex.rs),
  [`parse.rs`](../crates/pine-lite/src/parse.rs),
  [`typecheck.rs`](../crates/pine-lite/src/typecheck.rs), and
  [`limits.rs`](../crates/pine-lite/src/limits.rs) implement the front end.
- [`interp.rs`](../crates/pine-lite/src/interp.rs) runs the script and
  contains history, budgets, object caps, and `request.security` memoization.
- [`ta.rs`](../crates/pine-lite/src/ta.rs) adapts Pine's values to
  `analytics-core`; its `Option<f64>` outputs become Pine `na`.
- [`analytics-core/Cargo.toml`](../crates/analytics-core/Cargo.toml) and
  [`analytics-core/src/lib.rs`](../crates/analytics-core/src/lib.rs) enforce
  the pure, small, WASM-compatible math boundary.

### Improvement hypotheses to verify, not performance claims

1. **TA call-site work may be repeated across bars.** `Vm::series_arg` caches
   some source expressions in `series_cache`, but the `ta.*` dispatch in
   `interp.rs` invokes whole-series functions and selects `[self.bar]`.
   For example, `ta.sma(close, n)` computes a result vector while the script
   itself is being evaluated once per bar. This source shape suggests
   potentially repeated whole-window work and allocations; it is a benchmark
   hypothesis, not a measured result. A typed call-site ID and a safe cache
   for pure, immutable inputs—or a streaming execution plan—are candidates
   only after output-equivalence tests and timing/allocation measurements.
   Relevant locations: [`interp.rs`](../crates/pine-lite/src/interp.rs)
   (`series_cache`, `series_arg`, `ta.sma` dispatch) and
   [`ta.rs`](../crates/pine-lite/src/ta.rs).
2. **The AI/runtime capability contract is distributed.** `typecheck.rs`
   contains arity and multi-output matches, `interp.rs` contains runtime
   dispatch, and [`pine_codegen.rs`](../crates/api-gateway/src/pine_codegen.rs)
   contains model-facing guidance. A single descriptor schema plus parity
   tests would reduce drift without changing syntax.
3. **Server preview is not isolated by the existing WASM guest.**
   [`indicator_preview.rs`](../crates/api-gateway/src/indicator_preview.rs)
   calls `pine_lite::interp::run` in the gateway process. The existing
   [`sandbox-guest`](../crates/sandbox-guest/Cargo.toml) runs
   `strategy-runtime`/`strategy-dsl`, not Pine-Lite. The browser chart has a
   WASM artifact, which is not the same as a server process-isolation
   boundary. See [`docs/23`](./23-PINE-LITE-SCRIPTING-LANGUAGE.md) and
   [`docs/08`](./08-SANDBOX-WASM.md).
4. **Analytics ownership is directionally right, but parity still matters.**
   `ta.rs` delegates several functions to `analytics-core`, while other
   formulas remain in Pine-Lite. Preserve Pine-specific warm-up/session and
   `na` semantics; consolidate only after numerical fixtures prove parity.
5. **Current chart outputs already have a Rust contract.** The chart engine
   maps script plots/drawings into its scene models and owns coordinate
   conversion. It runs from the script spec and current candle window;
   [`frontend/chart-engine/src/script.rs`](../frontend/chart-engine/src/script.rs)
   is the relevant integration point, not a reason to replace the renderer.

## Repository inventory

Version and maturity labels below describe the cited source snapshot and the
release/history evidence checked on the research date. “Pattern only” means
the source informs design but is not proposed as a dependency.

### Languages and compiler infrastructure

| Project | Source snapshot / license | Source-level role and fit |
|---|---|---|
| Rhai | [`rhaiscript/rhai`](https://github.com/rhaiscript/rhai), `ff5f9873449b41b9c2efd958bf96496b08c3a1c7`, release v1.27.0; **MIT OR Apache-2.0** ([manifest](https://github.com/rhaiscript/rhai/blob/ff5f9873449b41b9c2efd958bf96496b08c3a1c7/Cargo.toml), [licenses](https://github.com/rhaiscript/rhai/tree/ff5f9873449b41b9c2efd958bf96496b08c3a1c7)). | Rust embedded dynamic language. `Engine` owns APIs/options ([source](https://github.com/rhaiscript/rhai/blob/ff5f9873449b41b9c2efd958bf96496b08c3a1c7/src/engine.rs#L92-L160)); resource limits include a configurable operation meter, not enabled by default ([source](https://github.com/rhaiscript/rhai/blob/ff5f9873449b41b9c2efd958bf96496b08c3a1c7/src/api/limits.rs#L38-L90)). Whole-language replacement duplicates Light Pine semantics: **no-go as replacement; limits are a pattern reference**. |
| Rune | [`rune-rs/rune`](https://github.com/rune-rs/rune), `bb8e69372353c50e271c9f115bc771c77aa6b83e`, manifest 0.15.0 / latest checked release 0.14.2; **MIT OR Apache-2.0** ([crate manifest](https://github.com/rune-rs/rune/blob/bb8e69372353c50e271c9f115bc771c77aa6b83e/crates/rune/Cargo.toml), [licenses](https://github.com/rune-rs/rune/tree/bb8e69372353c50e271c9f115bc771c77aa6b83e)). | Rust embeddable language with parse/compile/AST-HIR/bytecode VM layers ([entry point](https://github.com/rune-rs/rune/blob/bb8e69372353c50e271c9f115bc771c77aa6b83e/crates/rune/src/lib.rs#L17-L45), [parser](https://github.com/rune-rs/rune/blob/bb8e69372353c50e271c9f115bc771c77aa6b83e/crates/rune/src/parse/parser.rs#L28-L80)). Requires Rust 1.95 at the inspected head. **No-go as replacement; pattern-only for compilation/runtime separation.** |
| Chumsky | [Codeberg `zesterer/chumsky`](https://codeberg.org/zesterer/chumsky), `fc0e936803a8c9ee0857055575a54786a45d8959`, manifest 0.14.0 (tag evidence was older); **MIT** ([manifest](https://codeberg.org/zesterer/chumsky/src/commit/fc0e936803a8c9ee0857055575a54786a45d8959/Cargo.toml), [license](https://codeberg.org/zesterer/chumsky/src/commit/fc0e936803a8c9ee0857055575a54786a45d8959/LICENSE)). | Rust parser combinators; `Parser` and recovery APIs are in [`src/lib.rs`](https://codeberg.org/zesterer/chumsky/src/commit/fc0e936803a8c9ee0857055575a54786a45d8959/src/lib.rs#L324-L390) and [`recover_with`](https://codeberg.org/zesterer/chumsky/src/commit/fc0e936803a8c9ee0857055575a54786a45d8959/src/lib.rs#L1910-L1940). Parser benchmarks exist, but no Light Pine gain was established. **Pattern-only; consider a bounded parser prototype only if recovery tests justify it.** |
| Logos | [`maciejhirsz/logos`](https://github.com/maciejhirsz/logos), `fba6c4d4255bb213ca9f876f5ae8ab3a62c2433e`, release v0.16.1; **MIT OR Apache-2.0** ([manifest](https://github.com/maciejhirsz/logos/blob/fba6c4d4255bb213ca9f876f5ae8ab3a62c2433e/Cargo.toml), [licenses](https://github.com/maciejhirsz/logos/tree/fba6c4d4255bb213ca9f876f5ae8ab3a62c2433e)). | Rust token derive/codegen; lexer API and DFA implementation are in [`logos/src/lib.rs`](https://github.com/maciejhirsz/logos/blob/fba6c4d4255bb213ca9f876f5ae8ab3a62c2433e/logos/src/lib.rs#L8-L60) and [`logos-codegen/src/lib.rs`](https://github.com/maciejhirsz/logos/blob/fba6c4d4255bb213ca9f876f5ae8ab3a62c2433e/logos-codegen/src/lib.rs#L52-L80). It does not provide Light Pine's indentation/header rules, parser, or semantics. **Pattern-only; benchmark lexer in isolation before adoption.** |
| Pest | [`pest-parser/pest`](https://github.com/pest-parser/pest), `4eedd1a3ce32fa78da8a4a1dcf64d3f9ae4ba8d7`, release v2.9.6; **MIT OR Apache-2.0** ([manifest](https://github.com/pest-parser/pest/blob/4eedd1a3ce32fa78da8a4a1dcf64d3f9ae4ba8d7/pest/Cargo.toml), [licenses](https://github.com/pest-parser/pest/tree/4eedd1a3ce32fa78da8a4a1dcf64d3f9ae4ba8d7)). | Rust PEG parser/derive ([parser](https://github.com/pest-parser/pest/blob/4eedd1a3ce32fa78da8a4a1dcf64d3f9ae4ba8d7/pest/src/parser.rs#L1-L22)); parser-call limits are not whole-program execution budgets ([limit API](https://github.com/pest-parser/pest/blob/4eedd1a3ce32fa78da8a4a1dcf64d3f9ae4ba8d7/pest/src/parser_state.rs#L119-L130)). Inspected MSRV 1.88 exceeds QUANTERA 1.85. **No-go for replacing the current parser; no demonstrated need for a second grammar.** |
| Cranelift | Current source is the `cranelift/` subtree of [`bytecodealliance/wasmtime`](https://github.com/bytecodealliance/wasmtime/tree/3e0ee3fbfdd88d06c3f8c169168fe0d3b0c0096d/cranelift); source SHA `3e0ee3fbfdd88d06c3f8c169168fe0d3b0c0096d`; **Apache-2.0 WITH LLVM-exception** ([codegen manifest](https://github.com/bytecodealliance/wasmtime/blob/3e0ee3fbfdd88d06c3f8c169168fe0d3b0c0096d/cranelift/codegen/Cargo.toml)). | Rust machine-code backend; its codegen crate is separate from language parsing ([source](https://github.com/bytecodealliance/wasmtime/blob/3e0ee3fbfdd88d06c3f8c169168fe0d3b0c0096d/cranelift/codegen/src/lib.rs#L1-L70)). The old standalone repository is archived. QUANTERA already uses Cranelift inside Wasmtime; **no-go for a Light Pine JIT until a measured bottleneck and typed IR exist**. |
| Ariadne | [Codeberg `zesterer/ariadne`](https://codeberg.org/zesterer/ariadne), `299c95e66151039878c9879c0a2623e6218e3aa5`, manifest 0.7.0 (no tag verified); **MIT** ([manifest](https://codeberg.org/zesterer/ariadne/src/commit/299c95e66151039878c9879c0a2623e6218e3aa5/Cargo.toml), [license](https://codeberg.org/zesterer/ariadne/src/commit/299c95e66151039878c9879c0a2623e6218e3aa5/LICENSE)). | Rust diagnostic/report renderer; report and label API is in [`src/lib.rs`](https://codeberg.org/zesterer/ariadne/src/commit/299c95e66151039878c9879c0a2623e6218e3aa5/src/lib.rs#L1-L28). Could render structured Light Pine diagnostics without changing language semantics. **Best low-coupling pilot candidate**, after source byte spans and editor/source-cache needs are defined; not required in the interpreter. |
| Tree-sitter | [`tree-sitter/tree-sitter`](https://github.com/tree-sitter/tree-sitter), `ad0aeb42d34c9f922990cc1159b41dea37d9c0e7`, release v0.27.2 / workspace 0.28.0; **MIT** ([manifest](https://github.com/tree-sitter/tree-sitter/blob/ad0aeb42d34c9f922990cc1159b41dea37d9c0e7/Cargo.toml), [license](https://github.com/tree-sitter/tree-sitter/blob/ad0aeb42d34c9f922990cc1159b41dea37d9c0e7/LICENSE)). | C incremental parsing runtime ([parser](https://github.com/tree-sitter/tree-sitter/blob/ad0aeb42d34c9f922990cc1159b41dea37d9c0e7/lib/src/parser.c#L1-L25)) plus Rust generation/tooling. Useful for editor highlighting/navigation, not static typechecking. Current manifest MSRV 1.90. **Optional editor-only pattern; do not create a second authoritative compiler parser.** |
| Salsa | [`salsa-rs/salsa`](https://github.com/salsa-rs/salsa), `1f48f4a32954f27ddb0cd6fcbe8c42943cca3e9b`, release v0.28.5; **Apache-2.0 OR MIT** ([manifest](https://github.com/salsa-rs/salsa/blob/1f48f4a32954f27ddb0cd6fcbe8c42943cca3e9b/Cargo.toml), [licenses](https://github.com/salsa-rs/salsa/tree/1f48f4a32954f27ddb0cd6fcbe8c42943cca3e9b)). | Rust on-demand incremental-computation framework ([source](https://github.com/salsa-rs/salsa/blob/1f48f4a32954f27ddb0cd6fcbe8c42943cca3e9b/src/lib.rs#L1-L45)). Relevant only if an editor/LSP workload demonstrates repeated dependency recomputation; current manifest MSRV 1.95. **Needs evidence; not an immediate runtime dependency.** |
| Wasmtime | [`bytecodealliance/wasmtime`](https://github.com/bytecodealliance/wasmtime), `3e0ee3fbfdd88d06c3f8c169168fe0d3b0c0096d`, release v50.0.1; **Apache-2.0 WITH LLVM-exception** ([manifest](https://github.com/bytecodealliance/wasmtime/blob/3e0ee3fbfdd88d06c3f8c169168fe0d3b0c0096d/Cargo.toml), [license](https://github.com/bytecodealliance/wasmtime/blob/3e0ee3fbfdd88d06c3f8c169168fe0d3b0c0096d/LICENSE)). | Rust WebAssembly runtime with fuel ([config](https://github.com/bytecodealliance/wasmtime/blob/3e0ee3fbfdd88d06c3f8c169168fe0d3b0c0096d/crates/wasmtime/src/config.rs#L654-L675)), epoch interruption, and resource limiters ([limits](https://github.com/bytecodealliance/wasmtime/blob/3e0ee3fbfdd88d06c3f8c169168fe0d3b0c0096d/crates/wasmtime/src/runtime/limits.rs#L231-L356)). QUANTERA already pins Wasmtime 34 to retain Rust 1.85 and already uses fuel, time, and memory bounds for the strategy guest. **Retain; evaluate adding a Pine-Lite guest rather than upgrading or replacing the runtime.** |

### Technical analysis and quantitative computing

| Project | Source snapshot / license | Source-level role and fit |
|---|---|---|
| Wickra | [`wickra-lib/wickra`](https://github.com/wickra-lib/wickra), `306682227863d4af961116bb11f5c2d6c886f0d1`, v2.1.1 (2 Nov 2026); **MIT OR Apache-2.0** ([manifest](https://github.com/wickra-lib/wickra/blob/306682227863d4af961116bb11f5c2d6c886f0d1/Cargo.toml), [licenses](https://github.com/wickra-lib/wickra/tree/306682227863d4af961116bb11f5c2d6c886f0d1)). | Active Rust indicator workspace with stateful `update/reset`, warm-up `Option`, batch/caller-buffer APIs, and optional fast paths. Candle data is OHLCV; it also defines distinct trade/order-book types. Requires Rust 1.86 (one above workspace). **Closest API reference; adoption only after MSRV, semantics, dependencies, and benchmark review.** [traits](https://github.com/wickra-lib/wickra/blob/306682227863d4af961116bb11f5c2d6c886f0d1/crates/wickra-core/src/traits.rs) · [market data](https://github.com/wickra-lib/wickra/blob/306682227863d4af961116bb11f5c2d6c886f0d1/crates/wickra-core/src/microstructure.rs) |
| VectorTA | [`VectorAlpha-dev/VectorTA`](https://github.com/VectorAlpha-dev/VectorTA), `802518e2392c5d011744b75e56e108e97a0682b4`, v0.3.1 (18 Jul 2026); **Apache-2.0** ([manifest](https://github.com/VectorAlpha-dev/VectorTA/blob/802518e2392c5d011744b75e56e108e97a0682b4/Cargo.toml), [license](https://github.com/VectorAlpha-dev/VectorTA/blob/802518e2392c5d011744b75e56e108e97a0682b4/LICENSE)). | Rust batch and stream indicator APIs, with AVX2/AVX512 paths for some indicators. Its wider dependency set includes ndarray, Rayon, and optional language/GPU bindings. Criterion workload sizes (10k/100k/1M candles) are declared but no result was independently reproduced. **Pattern-only; no leaf dependency without per-feature parity review.** [RSI](https://github.com/VectorAlpha-dev/VectorTA/blob/802518e2392c5d011744b75e56e108e97a0682b4/src/indicators/rsi.rs) · [benchmark harness](https://github.com/VectorAlpha-dev/VectorTA/blob/802518e2392c5d011744b75e56e108e97a0682b4/benches/indicator_benchmark.rs) |
| YATA | [`amv-dev/yata`](https://github.com/amv-dev/yata), `e331b48243f32b3817c6f97240c6be0188b90c6a`, v0.7.0 (19 Sep 2024); **Apache-2.0** ([manifest](https://github.com/amv-dev/yata/blob/e331b48243f32b3817c6f97240c6be0188b90c6a/Cargo.toml), [license](https://github.com/amv-dev/yata/blob/e331b48243f32b3817c6f97240c6be0188b90c6a/LICENSE)). | Config/instance split; `next` streams OHLCV into fixed/multi-value `IndicatorResult`, and `over` processes slices. The inspected head is older than the active candidates. RSI is scaled 0–1, unlike common Pine conventions. **Strong structural reference; test output scaling and warm-ups before adoption.** [config](https://github.com/amv-dev/yata/blob/e331b48243f32b3817c6f97240c6be0188b90c6a/src/core/indicator/config.rs) · [instance](https://github.com/amv-dev/yata/blob/e331b48243f32b3817c6f97240c6be0188b90c6a/src/core/indicator/instance.rs) |
| TA-Lib | [`TA-Lib/ta-lib`](https://github.com/TA-Lib/ta-lib), `fd57d4cfd6c812004d897feb0025fe273b1192e2`, source 0.8.4; **BSD-3-Clause (SPDX)** ([license text and conditions](https://github.com/TA-Lib/ta-lib/blob/fd57d4cfd6c812004d897feb0025fe273b1192e2/LICENSE#L3-L19)). | Mature C library with broad batch TA APIs and a genuine stateful RSI `Open/Update/Peek/Close/Value` interface. FFI/build portability and Pine warm-up semantics matter, including WASM. **Formula/differential-test reference; not a direct Rust dependency recommendation.** [RSI API](https://github.com/TA-Lib/ta-lib/blob/fd57d4cfd6c812004d897feb0025fe273b1192e2/include/ta_func.h) · [RSI implementation](https://github.com/TA-Lib/ta-lib/blob/fd57d4cfd6c812004d897feb0025fe273b1192e2/src/ta_func/ta_RSI.c) |
| ta-rs | Name is ambiguous. Candidate inspected: [`greyblake/ta-rs`](https://github.com/greyblake/ta-rs), `ecd65d85def675e578ddd3c7187fff2b6fad28fc`, crate `ta` 0.5.0 (2022); **MIT** ([manifest](https://github.com/greyblake/ta-rs/blob/ecd65d85def675e578ddd3c7187fff2b6fad28fc/Cargo.toml), [license](https://github.com/greyblake/ta-rs/blob/ecd65d85def675e578ddd3c7187fff2b6fad28fc/LICENSE)). | Small, quiet stateful `Next<T>` API over scalar/OHLCV inputs. EMA starts from the first value, unlike QUANTERA's SMA-seeded EMA. **Do not assume this is the intended repo; pattern only pending identity and semantic review.** [EMA](https://github.com/greyblake/ta-rs/blob/ecd65d85def675e578ddd3c7187fff2b6fad28fc/src/indicators/exponential_moving_average.rs) |
| Polars | [`pola-rs/polars`](https://github.com/pola-rs/polars), `a9bce976c1fcac7f5c8f284f7cfc33262175f386`, v0.55.1; **MIT** ([manifest](https://github.com/pola-rs/polars/blob/a9bce976c1fcac7f5c8f284f7cfc33262175f386/Cargo.toml), [license](https://github.com/pola-rs/polars/blob/a9bce976c1fcac7f5c8f284f7cfc33262175f386/LICENSE)). | Columnar/DataFrame query and chunk-stream engine, not a per-indicator event-state API. Broad Rust-2024 workspace is disproportionate for the analytics leaf. **Offline batch/research use only if separately justified.** [EWM](https://github.com/pola-rs/polars/blob/a9bce976c1fcac7f5c8f284f7cfc33262175f386/crates/polars-ops/src/series/ops/ewm.rs) · [stream morsel](https://github.com/pola-rs/polars/blob/a9bce976c1fcac7f5c8f284f7cfc33262175f386/crates/polars-stream/src/morsel.rs) |
| ndarray | [`rust-ndarray/ndarray`](https://github.com/rust-ndarray/ndarray), `53df84ade302c5d4d49b91aa6e4a18d252d26cc2`, v0.17.2; **MIT OR Apache-2.0** ([manifest](https://github.com/rust-ndarray/ndarray/blob/53df84ade302c5d4d49b91aa6e4a18d252d26cc2/Cargo.toml), [licenses](https://github.com/rust-ndarray/ndarray/tree/53df84ade302c5d4d49b91aa6e4a18d252d26cc2)). | General dense N-dimensional arrays/views; optional Rayon/BLAS. MSRV 1.87. **Use only for an isolated multi-dimensional feature/model workload, not basic series TA.** [array types](https://github.com/rust-ndarray/ndarray/blob/53df84ade302c5d4d49b91aa6e4a18d252d26cc2/src/lib.rs) |
| nalgebra | [`dimforge/nalgebra`](https://github.com/dimforge/nalgebra), `b2466e6c4070b06240d929895473c9141fe24720`, v0.35.0; **Apache-2.0** ([manifest](https://github.com/dimforge/nalgebra/blob/b2466e6c4070b06240d929895473c9141fe24720/Cargo.toml), [license](https://github.com/dimforge/nalgebra/blob/b2466e6c4070b06240d929895473c9141fe24720/LICENSE)). | Generic matrix/vector algebra, no market-data contract; MSRV 1.89. **Only for concrete matrix/risk calculations in a bounded crate.** [matrix aliases](https://github.com/dimforge/nalgebra/blob/b2466e6c4070b06240d929895473c9141fe24720/src/base/alias.rs) |
| statrs | [`statrs-dev/statrs`](https://github.com/statrs-dev/statrs), `55dfea2d53cd3fe6523f1ea6f08ec051d1c362f1`, v0.19.1; **MIT** ([manifest](https://github.com/statrs-dev/statrs/blob/55dfea2d53cd3fe6523f1ea6f08ec051d1c362f1/Cargo.toml), [license](https://github.com/statrs-dev/statrs/blob/55dfea2d53cd3fe6523f1ea6f08ec051d1c362f1/LICENSE)). | Distribution/statistics library; `OnlineMoments` implements one-pass mergeable moments. Default dependencies include nalgebra/rand; MSRV 1.89. **Use online algorithms as references; add a dependency only for a named feature.** [Online moments](https://github.com/statrs-dev/statrs/blob/55dfea2d53cd3fe6523f1ea6f08ec051d1c362f1/src/statistics/online.rs) |
| RustFFT | [`ejmahler/RustFFT`](https://github.com/ejmahler/RustFFT), `26a8ae71cb41811bf0c270edc739b0b93bec125c`, v6.4.1; **MIT OR Apache-2.0** ([manifest](https://github.com/ejmahler/RustFFT/blob/26a8ae71cb41811bf0c270edc739b0b93bec125c/Cargo.toml), [licenses](https://github.com/ejmahler/RustFFT/tree/26a8ae71cb41811bf0c270edc739b0b93bec125c)). | Planned FFTs with reusable plans/scratch; no OHLCV contract; MSRV 1.88. **Optional only for a specific spectral feature.** [planner](https://github.com/ejmahler/RustFFT/blob/26a8ae71cb41811bf0c270edc739b0b93bec125c/src/plan.rs) |
| Linfa | [`rust-ml/linfa`](https://github.com/rust-ml/linfa), `3b411a4c985cdf6b83430d9c44f11e338d300c50`, v0.8.1; **MIT OR Apache-2.0** ([manifest](https://github.com/rust-ml/linfa/blob/3b411a4c985cdf6b83430d9c44f11e338d300c50/Cargo.toml), [licenses](https://github.com/rust-ml/linfa/tree/3b411a4c985cdf6b83430d9c44f11e338d300c50)). | ML toolkit over ndarray datasets; fitting is not per-market-event indicator streaming; optional BLAS/native integrations. **Keep out of Pine runtime; separate model/research scope if needed.** [Fit traits](https://github.com/rust-ml/linfa/blob/3b411a4c985cdf6b83430d9c44f11e338d300c50/src/traits.rs) |
| Burn | [`tracel-ai/burn`](https://github.com/tracel-ai/burn), `26f664754fdeb5d9cbe4970d80185e329dd978c6`, v0.23.0-pre.2; **MIT OR Apache-2.0** ([manifest](https://github.com/tracel-ai/burn/blob/26f664754fdeb5d9cbe4970d80185e329dd978c6/Cargo.toml), [license](https://github.com/tracel-ai/burn/blob/26f664754fdeb5d9cbe4970d80185e329dd978c6/LICENSE-MIT)). | Tensor/autograd/device framework, active pre-release, no TA or OHLCV contract. **Separate ML work only; not a script-runtime dependency.** [Tensor API](https://github.com/tracel-ai/burn/blob/26f664754fdeb5d9cbe4970d80185e329dd978c6/crates/burn-tensor/src/tensor/api/base.rs) |

### Trading, charting, and AI infrastructure

| Project | Source snapshot / license | Source-level role and fit |
|---|---|---|
| NautilusTrader | [`nautechsystems/nautilus_trader`](https://github.com/nautechsystems/nautilus_trader), `a8786259f83c9787293adc456444bfd38c361df6`, release v1.231.0; **LGPL-3.0** ([release](https://github.com/nautechsystems/nautilus_trader/releases/tag/v1.231.0), [license](https://github.com/nautechsystems/nautilus_trader/blob/a8786259f83c9787293adc456444bfd38c361df6/LICENSE)). | Rust engine with Python bindings, ordered replay states and explicit submitted/accepted/filled event types. Rust toolchain 1.99.1 and LGPL make it a poor drop-in. **Pattern-only: replay state and order lifecycle; preserve QUANTERA execution and risk.** [Replay iterator](https://github.com/nautechsystems/nautilus_trader/blob/a8786259f83c9787293adc456444bfd38c361df6/crates/data/src/replay/iterator.rs) · [matching](https://github.com/nautechsystems/nautilus_trader/blob/a8786259f83c9787293adc456444bfd38c361df6/crates/execution/src/matching.rs) |
| Barter | [`barter-rs/barter-rs`](https://github.com/barter-rs/barter-rs), `9770b27a83f844472b93b593b08063affc974b0d`, `barter` v0.14.0; **MIT** ([manifest](https://github.com/barter-rs/barter-rs/blob/9770b27a83f844472b93b593b08063affc974b0d/barter/Cargo.toml), [license](https://github.com/barter-rs/barter-rs/blob/9770b27a83f844472b93b593b08063affc974b0d/LICENSE)). | Rust event-driven feed/strategy/risk interfaces and approved/refused risk results. Main package has no matching latest release; macro release exists. **Pattern-only; avoid a second event/order/risk stack.** [Backtest](https://github.com/barter-rs/barter-rs/blob/9770b27a83f844472b93b593b08063affc974b0d/barter/src/backtest/mod.rs) · [risk](https://github.com/barter-rs/barter-rs/blob/9770b27a83f844472b93b08063affc974b0d/barter/src/risk/mod.rs) |
| HftBacktest | [`nkaz001/hftbacktest`](https://github.com/nkaz001/hftbacktest), `5f3ec40b2afb764e0fea112f941ed85523ef4e88`, v0.9.4; **MIT** ([release](https://github.com/nkaz001/hftbacktest/releases/tag/rust-v0.9.4), [manifest](https://github.com/nkaz001/hftbacktest/blob/5f3ec40b2afb764e0fea112f941ed85523ef4e88/hftbacktest/Cargo.toml), [license](https://github.com/nkaz001/hftbacktest/blob/5f3ec40b2afb764e0fea112f941ed85523ef4e88/LICENSE)). | Rust/Python event simulation with configurable risk-adverse/probabilistic/L3 queue models, latencies, fees, and depth. Rust 1.91.1 exceeds QUANTERA's MSRV. **Pattern-only for a distinct, future L2/L3 execution simulator; candle fills cannot support queue-position claims.** [Queue models](https://github.com/nkaz001/hftbacktest/blob/5f3ec40b2afb764e0fea112f941ed85523ef4e88/hftbacktest/src/backtest/models/queue.rs) |
| TradeRyte Trading Charts | **Unresolved.** GitHub repository searches did not identify a public source repository matching the supplied name. No revision, license, source architecture, or performance claims can be verified. | Do not guess a lookalike or include it in dependency/compatibility decisions. A URL or owner/repository is needed to continue this item. |
| TradingView Lightweight Charts | [`tradingview/lightweight-charts`](https://github.com/tradingview/lightweight-charts), `114b50af78808c9632873913422639dd6931480a`, v5.2.1; **Apache-2.0** ([release](https://github.com/tradingview/lightweight-charts/releases/tag/v5.2.1), [license](https://github.com/tradingview/lightweight-charts/blob/114b50af78808c9632873913422639dd6931480a/LICENSE)). | TypeScript/JavaScript canvas chart with explicit time/price transforms, monotonic streaming-update rules, and attachable drawing primitives. **Pattern-only; QUANTERA already has a Rust scene/coordinate contract.** [Series API](https://github.com/tradingview/lightweight-charts/blob/114b50af78808c9632873913422639dd6931480a/src/api/iseries-api.ts) · [primitive API](https://github.com/tradingview/lightweight-charts/blob/114b50af78808c9632873913422639dd6931480a/src/model/iseries-primitive.ts) |
| Plotters | [`plotters-rs/plotters`](https://github.com/plotters-rs/plotters), `c63248e0cf7c611185361ac492003e8a87d52ea8`; **MIT** ([manifest](https://github.com/plotters-rs/plotters/blob/c63248e0cf7c611185361ac492003e8a87d52ea8/Cargo.toml), [license](https://github.com/plotters-rs/plotters/blob/c63248e0cf7c611185361ac492003e8a87d52ea8/LICENSE)). Latest-release endpoint returned 404 when checked. | Rust plotting/backend library with explicit coordinate translation and frame presentation; not an interactive time-series scene engine. **Pattern/reference or export use case, not renderer replacement.** [coordinate traits](https://github.com/plotters-rs/plotters/blob/c63248e0cf7c611185361ac492003e8a87d52ea8/plotters/src/coord/translate.rs) · [backend](https://github.com/plotters-rs/plotters/blob/c63248e0cf7c611185361ac492003e8a87d52ea8/plotters-backend/src/lib.rs) |
| MCP Rust SDK | Official [`modelcontextprotocol/rust-sdk`](https://github.com/modelcontextprotocol/rust-sdk), `f2b0ddf113d1ed426b77a81511eac6e4bffa0ac4`, `rmcp` v3.9.0; package says **Apache-2.0**, while LICENSE records legacy MIT contributions and CC-BY-4.0 documentation terms ([release](https://github.com/modelcontextprotocol/rust-sdk/releases/tag/rmcp-v3.9.0), [manifest](https://github.com/modelcontextprotocol/rust-sdk/blob/f2b0ddf113d1ed426b77a81511eac6e4bffa0ac4/crates/rmcp/Cargo.toml), [license](https://github.com/modelcontextprotocol/rust-sdk/blob/f2b0ddf113d1ed426b77a81511eac6e4bffa0ac4/LICENSE)). | Rust protocol SDK for typed tool/resource handlers; `rmcp` MSRV 1.88 exceeds QUANTERA's 1.85. **Optional adapter layer only; keep generation, validation, replay, and install behind existing gateway/services.** [tool router](https://github.com/modelcontextprotocol/rust-sdk/blob/f2b0ddf113d1ed426b77a81511eac6e4bffa0ac4/crates/rmcp/src/handler/server/router/tool.rs) · [server handlers](https://github.com/modelcontextprotocol/rust-sdk/blob/f2b0ddf113d1ed426b77a81511eac6e4bffa0ac4/crates/rmcp/src/handler/server.rs) |

## Comparative investigations

### A. Light Pine language and compiler

| Concern | Evidence from references | Proposed Light Pine direction |
|---|---|---|
| Lexer/parser | Logos generates lexers; Chumsky offers composable parsers/recovery; Pest generates PEG parsers; Tree-sitter supports incremental editor parsing. They are alternatives or editor tools, not drop-in semantic layers. | Keep the handwritten lexer/parser until a measured error-recovery or maintenance failure justifies an isolated experiment. Preserve indentation/newline rules and existing syntax. Tree-sitter, if needed, is editor-only and non-authoritative. |
| AST/type checking | Rhai and Rune demonstrate that full embedded languages own their AST, compiler, host API, and runtime. They do not add one useful component to the existing Pine AST. | Keep Light Pine as the only scripting language. First enrich `ScriptError`/checked output with byte spans, diagnostic codes, related spans, expected/actual types, and repair hints. |
| IR/runtime | Rune/Rhai separate source compilation and VM execution; Cranelift is a low-level codegen backend. YATA/Wickra/TA-Lib show stateful `update` shapes. | Introduce a verified compile plan/typed IR only when benchmarks justify it. Give callsites stable IDs; precompute pure series once and maintain mutable streaming state for truly incremental expressions. Do not start with native JIT. |
| Resource limits | Rhai's operation meter is configurable and disabled by default; parser call/stack limits are parser safeguards. Wasmtime source exposes fuel, epoch, and memory/table limits. | Continue static language budgets. Measure kernel work separately from AST steps. If server isolation is required, execute the repository-built Pine interpreter as a Wasm guest with narrowly allowlisted market-data inputs and bounded output; never give scripts filesystem/network/clock/broker access. |
| Diagnostics | Chumsky can recover with error nodes; Ariadne renders labeled source diagnostics. | Make structured diagnostics language-owned and protocol-stable; use Ariadne only as an optional presentation adapter after byte ranges/source context exist. |
| Modules/security | Rhai/Rune provide embedded module/host API models; they also expose much larger language/runtime surfaces. | Keep the script contract closed: versioned builtins and user functions, no arbitrary imports or host calls. Add module namespaces only when standard-library ownership is clear. |

### B. Indicator infrastructure and proposed API

The libraries offer different interfaces and semantics; no function-by-function
result should be assumed numerically interchangeable. Wickra's explicit
stateful update/reset and batch/caller-buffer APIs, YATA's config/instance
split, TA-Lib's batch plus RSI stream, and VectorTA's batch/SIMD/stream paths
are the useful patterns. Their warm-up, EMA seeds, RSI scale, `na` behavior,
and data inputs differ. YATA's RSI is 0–1; `ta-rs`'s EMA seeds from the first
value; Pine-Lite's EMA is SMA-seeded. Differential tests must name those
differences rather than treating a passing formula name as parity.

Source-verified coverage is illustrated below; it is not an exhaustive count
of each library's full catalogue:

| Project | Verified examples and implementation details |
|---|---|
| Wickra | RSI, ATR, MACD, and Bollinger Bands; MACD updates statefully and returns three outputs. Trade imbalance and top-of-book imbalance consume trade and order-book inputs, respectively ([indicators](https://github.com/wickra-lib/wickra/tree/306682227863d4af961116bb11f5c2d6c886f0d1/crates/wickra-core/src/indicators)). |
| VectorTA | RSI, ATR, MACD, Bollinger Bands, volume-zone oscillator, and stochastic money-flow index. MACD has a three-series result and stream API; inspected AVX kernels are gated behind `nightly-avx` on x86_64 ([indicators](https://github.com/VectorAlpha-dev/VectorTA/tree/802518e2392c5d011744b75e56e108e97a0682b4/src/indicators)). |
| YATA | RSI, MACD, Bollinger Bands, Money Flow Index, and ADX. The inspected API streams OHLCV; MACD emits two values and two signals, Bollinger emits three values and one signal, and RSI is scaled 0–1 ([indicator sources](https://github.com/amv-dev/yata/tree/e331b48243f32b3817c6f97240c6be0188b90c6a/src/indicators)). |
| TA-Lib | RSI, ATR, MACD, Bollinger Bands, OBV, and candlestick patterns. Persistent stream APIs were verified for RSI, ATR, OBV, and CDLDOJI; RSI exposes `Open/Update/Peek/Close/Value` ([indicator sources](https://github.com/TA-Lib/ta-lib/tree/fd57d4cfd6c812004d897feb0025fe273b1192e2/src/ta_func), [stream API](https://github.com/TA-Lib/ta-lib/blob/fd57d4cfd6c812004d897feb0025fe273b1192e2/include/ta_func.h#L17231-L17256)). |
| `greyblake/ta-rs` candidate | SMA, RSI, ATR, MACD, Bollinger Bands, and OBV. `Next<T>` is stateful; MACD and Bollinger return three values. “ta-rs” remains ambiguous, so this is not confirmed as the intended target ([indicator sources](https://github.com/greyblake/ta-rs/tree/ecd65d85def675e578ddd3c7187fff2b6fad28fc/src/indicators)). |

**Proposed internal contract (not yet implemented):**

- `IndicatorSpec`: stable name/version, parameter schema and constraints,
  input fields/data granularity, output names/types, multi-output count,
  warm-up policy, reset/session policy, and complexity/resource class.
- `IndicatorState`: validated configuration plus bounded state. Its update
  consumes one candle/trade/book event and returns typed outputs; reset is
  explicit. No I/O and no per-update allocation for normal fixed-size kernels.
- Batch adapter: processes slices through the same streaming kernel when
  semantics agree, while separately documenting optimized batch
  implementations.
- Pine adapter: maps output fields to Pine `float`/`bool` plus `na` and
  exposes consistent positional/named parameters. One descriptor registry
  should drive typecheck arity/output checks, AI capability docs, fixtures,
  and install validation. Runtime dispatch must be tested against the
  descriptors.
- Tests: golden numerical vectors; warm-up/zero/invalid parameter handling;
  batch-vs-stream parity; session reset; prefix/causality checks; finite output
  and overflow handling; native/WASM parity.

Do not wholesale import an indicator crate into `pine-lite`: it is a WASM
leaf, and dependency/MSRV/API costs vary. Keep `analytics-core` canonical.
Use TA-Lib/YATA/Wickra/VectorTA as independent formula or interface references
where licenses and semantics permit; do not copy source without a separate
license/provenance review.

### C. Quantitative intelligence and data requirements

QUANTERA's `analytics-core` already owns Candle, Trade, OrderBookSnapshot,
Footprint, CVD, VWAP, profiles, and regime-related calculations. Do not
reimplement them in Pine-Lite. Expose a feature only when the host can supply
the required data and its time alignment is explicit.

| Capability | Minimum valid input | QUANTERA/source implication |
|---|---|---|
| SMA/EMA, ATR, return volatility, bands, most candle structure | OHLC; volume where volume-weighted | OHLCV can support these. Warm-up, seeding, gaps, and prefix causality need tests. |
| VWAP | Candle typical price × volume is an approximation; raw trade price × quantity is exact over the supplied trade window | `analytics-core/src/vwap.rs` exposes both. Pine's per-bar/session VWAP must not broadcast a full-window final value backward. |
| CVD/delta | Buyer-/seller-aggressed quantities per bar, or signed trades | QUANTERA `Candle` carries `buy_volume`/`sell_volume`; `Trade::signed_quantity`, `Cvd::update`, and session reset are available. Plain OHLCV without aggressor split cannot recover true delta. |
| Footprint and price-level imbalance | Individual trades with price, size, aggressor side, and bucket policy | `FootprintCandle` and diagonal imbalance logic consume trade-derived price-level cells. OHLCV alone cannot reconstruct the footprint. |
| Volume profile / POC / value area | Raw trade prices/quantities for exact profile; candle profile is approximate | Core has a trade-based calculation and an explicitly approximate candle-spreading path. Advertise the approximation as such. |
| Order-book imbalance, spread, depth dynamics | Timestamped bid/ask levels; for dynamics, ordered book updates/trades | `OrderBookSnapshot` is a data type; the inspected general analytics API did not establish a general live depth-imbalance series. Define and test this in analytics-core before exposing it. |
| Queue position and fill probability | L2/L3 snapshots or event deltas, trades, order timing/latency | HftBacktest has queue models; candles are insufficient. Keep this in a separate execution simulation, not as an indicator assertion. |
| Statistics/regime | OHLCV returns for many simple statistics; more state/features for probabilistic regimes | `analytics-core` already has statistical/regime code. `statrs::OnlineMoments` is a pattern; ML libraries do not replace deterministic indicator contracts. |
| Signal processing | Regularly sampled numeric series; explicit window/phase/latency policy | RustFFT is a candidate only for a specified spectral feature. FFT transforms do not automatically imply a causal indicator. |

### D. Trading execution

NautilusTrader provides explicit replay states, order events, and matching;
Barter provides event-loop and generic strategy/risk boundaries; HftBacktest
models queues, latency, fees, and book depth. They answer different questions.
Their benchmark declarations are not comparable performance results.

QUANTERA already has a deterministic candle replay, Pine strategy adapter,
simulator, `trading-engine` risk checks, and live gate. Preserve these as
authorities. If order-book-level simulation is required, introduce a distinct
data-backed simulator with documented assumptions (partial fills, queue
position, latency, fees) and conformance tests; do not claim candle replay has
HFT execution fidelity. NautilusTrader's LGPL-3.0, current Rust 1.99 toolchain,
and HftBacktest's Rust 1.91 MSRV are additional adoption constraints.

### E. Chart rendering contract

Lightweight Charts is useful for monotonic update rules, time/price coordinate
conversion, and attachable primitive lifecycles. Plotters demonstrates backend
and coordinate traits, but is a static plotting abstraction. TradeRyte could
not be verified. QUANTERA already owns a Rust/WASM chart scene contract and
maps script values to pane/overlay coordinates in
[`chart-engine/src/script.rs`](../frontend/chart-engine/src/script.rs).

**Recommended contract for future work:** retain renderer-owned pixel
coordinates. Script output uses stable series/object IDs; value arrays are
bar/time indexed; drawings carry semantic anchors (bar index or absolute
timestamp plus price); each series declares pane/scale/style; updates specify
replace-window, append, or amend-last-bar; object creation/deletion and
per-kind/global caps are deterministic. Only add profiles/heatmaps/panels when
an actual feature needs them. Keep scene generation, coordinate transforms,
and painting separated as they are today.

### F. AI agent integration

MCP's `rmcp` supplies typed tool/resource protocol plumbing; it is not the
language runtime or an authorization system. The correct boundary is a thin
adapter to existing QUANTERA services. If adopted (after Rust MSRV/license
review), expose read-only resources for:

1. `pine_lite.capabilities` — versioned syntax, builtin descriptors, data
   requirements, chart outputs, and strategy/indicator restrictions.
2. `pine_lite.examples` — verified source + golden data/output fixtures.

Provide bounded tools for `validate_script`, `run_indicator_fixture`,
`preview_script`, `run_backtest`, and `install_validated_script`. Return
machine-readable diagnostics and measurable output evidence. Installation
requires vetting and preview/test success. The MCP tool must not give an
indicator script direct broker methods; strategies still pass through the
existing risk engine and live gate.

## License compatibility matrix

| Projects | Verified license(s) | Preliminary fit |
|---|---|---|
| Rhai, Rune, Logos, Pest, Salsa, ndarray, RustFFT, Linfa, Burn | MIT OR Apache-2.0 (Salsa: Apache-2.0 OR MIT) | Common permissive terms, but check exact dependency/features and retain notices. Several current versions exceed MSRV. |
| Chumsky, Ariadne, Polars, Barter, Plotters, `greyblake/ta-rs`, Wickra | MIT (Wickra is MIT OR Apache-2.0) | Common permissive terms; still no reason to adopt unused code. Chumsky/Ariadne source is now Codeberg. |
| VectorTA, YATA, nalgebra | Apache-2.0 | Permissive with notice/license obligations; review dependencies/features. |
| TA-Lib | BSD-3-Clause (SPDX) | Permissive with source/binary notice and endorsement restrictions. |
| Cranelift, Wasmtime | Apache-2.0 WITH LLVM-exception | QUANTERA already uses Wasmtime under its pinned version; review notices on any version/feature change. |
| NautilusTrader | LGPL-3.0 | Higher diligence for linking/distribution; pattern study is preferable to dependency adoption. |
| Lightweight Charts | Apache-2.0 | A TypeScript renderer dependency would still duplicate the Rust scene boundary. |
| Official MCP Rust SDK (`rmcp`) | Package says Apache-2.0; LICENSE documents legacy MIT code contributions and CC-BY-4.0 documentation terms | Review the actual crate/source and documentation assets before adoption; current MSRV is also above QUANTERA's. |
| TradeRyte Trading Charts | Unverified | No license or compatibility conclusion possible without exact repository identity. |

The table is not a transitive license audit. The least-risk course is no new
dependencies until a concrete feature, version/MSRV fit, full dependency tree,
license notices, build targets, and measured benefit are reviewed.

## Performance comparison and benchmark policy

There is no defensible speed leaderboard across these projects: they measure
different operations, data sizes, languages, hardware, and batch-vs-stream
workloads. The limited evidence is:

- Wickra publishes its own result for a Python one-at-a-time SMA(20) workload:
  after a 5,000-bar seed and 10,000 appended ticks, it reports 0.061 μs for
  Wickra versus 234 μs for TA-Lib recomputed over history. This is
  project-reported on Windows 11 / Ryzen 9 9950X / Rust 1.92 and compares
  incremental update with repeated batch recomputation; it is **not** an
  apples-to-apples algorithm comparison or independent reproduction.
  [Wickra benchmark methodology/results](https://github.com/wickra-lib/wickra/blob/306682227863d4af961116bb11f5c2d6c886f0d1/BENCHMARKS.md)
- VectorTA has a Criterion harness for 10k/100k/1M-candle workloads; inspected
  source establishes test sizes, not a verified published numeric result.
- RustFFT benchmarks planned FFTs and scratch-buffer reuse, which is relevant
  to FFT workloads but not evidence for rolling OHLCV indicators.
- NautilusTrader and Barter declare Criterion benchmarks. A harness declaration
  is not a throughput or latency result.
- Parser projects' benchmark targets and source-level DFA/PEG designs do not
  predict Light Pine's parsing or AI repair latency.

**QUANTERA baseline to establish before optimization:** identical release build,
same candle fixtures, same inputs, and same script source; report cold vet,
warm execution, bars/second, p50/p95 duration, allocations/peak memory,
interpreter steps, and output equality. Cover 500/2k/10k bars, 1/5/20 TA
call-sites, nested TA expressions, stateful generated strategies,
multi-timeframe/security requests, and drawing-heavy scripts. Test append/
amend-last-bar workloads separately from full-window replay. Compare only
implementations with equivalent warm-up and output semantics. Include prefix
invariance/no-lookahead and native/WASM parity as correctness gates; do not
trade them for speed.

## Recommended Light Pine 2.0 architecture

1. **Versioned language contract:** continue Pine-like series/closed-bar
   semantics, explicit `na`, bounded state, named input/data contracts,
   strategy simulation separated from broker execution. Every new behavior
   gets a language-version decision and compatibility fixture.
2. **Authoritative builtin registry:** one Rust descriptor per callable name
   with ID, arguments/defaults, type/output shape, Pine namespace, required
   data, warm-up/reset behavior, cost class, and docs/examples. Generate or
   validate AI reference, typechecker arity/output checks, capability
   discovery, and tests from it. Keep mathematical implementation in
   `analytics-core`.
3. **Structured compiler diagnostics:** errors carry stable codes, severity,
   byte ranges, line/column, expected/actual values, related spans, help, and
   repair context. Map to the current API shape during migration. Ariadne is
   an optional renderer; do not couple the runtime to a terminal formatter.
4. **Compile-once execution plan:** introduce only after baseline. Resolve
   names/calls/types into stable IDs and an internal typed IR, then run that
   plan across bars. Cache only pure computations over immutable inputs;
   mutable series/state uses a streaming kernel. Keep a reference interpreter
   during equivalence rollout. Cranelift remains deferred until the IR and
   benchmarks justify another backend.
5. **Unified indicator kernel contract:** statically dispatched,
   no-I/O kernels with validated config, explicit `update/reset`, output
   schema, optional warm-up and per-session reset. Batch paths either reuse
   the stream implementation or prove numerical equivalence. SIMD is an
   optional target-specific optimization after a scalar oracle and benchmark.
6. **Data capability tiers:** every API declares OHLC/volume, aggressor
   volume/trades, or order-book requirements. The host resolves and aligns
   them; scripts cannot fetch arbitrary resources. Approximate candle-derived
   statistics are labeled as approximations.
7. **Isolation boundary:** for server preview, build the trusted Rust Pine
   interpreter into a WASM guest and send source/validated representation,
   candles, inputs, and bounded outputs as data. Reuse Wasmtime 34 first;
   preserve a small import allowlist and fuel/time/memory limits. Add explicit
   native-vs-WASM fixtures, adversarial tests, and measured resource budgets.
   Do not expose broker or network capabilities. The browser WASM artifact
   remains a separate chart runtime.
8. **Chart output ABI:** keep semantic series/anchors in the scene contract,
   version stable IDs and update operations, and leave pixel mapping/rendering
   in the Rust chart engine.
9. **AI Developer Kit:** expose capability/resources and validation/preview/
   backtest/install tools through gateway APIs; optionally bridge via MCP.
   Tool results return source hash, compiler version, diagnostics, fixture/
   replay stats, and chart output counts. Model prose never substitutes for
   code/output evidence.
10. **Optional editor tooling:** Tree-sitter for highlighting/incremental
    syntax only if a real editor workflow needs it; Salsa only if incremental
    compile profiling demonstrates value. Neither becomes another source of
    truth.

## Prioritized implementation roadmap (report-first; no changes made here)

| Priority | Work item | Exit evidence |
|---|---|---|
| P0 | Reconcile authoritative capability/data/security matrix; capture current preview/backtest/chart routes and their source-of-truth status. | Each advertised capability links to implementation + tests; direct gateway execution and server guest boundaries are explicit. |
| P1 | Establish golden numerical and causal fixtures for existing functions; record EMA/RMA/RSI/ATR/VWAP warm-up and session rules. | Prefix-invariance, parity, and `na` behavior are pinned before algorithm consolidation. |
| P2 | Add representative QUANTERA runtime/allocation benchmark harness; include TA-heavy script and append/replay modes. | Reproducible baseline with matching outputs and environment/build details. |
| P3 | Design the builtin descriptor schema and prove parity with current checker/runtime/prompt. | No listed function is missing or falsely advertised; AI capabilities are generated or test-verified. |
| P4 | Prototype per-callsite output caching or typed execution plan for one measured hot path. | Before/after identical fixtures, no lookahead, and repeatable speed/memory improvement; retain fallback. |
| P5 | Compare streaming indicator kernels and consolidate only proven parity families into `analytics-core`. | Batch/stream and Pine/core golden tests, WASM build, bounded memory. |
| P6 | Design/implement Pine-Lite guest isolation using current Wasmtime version if server risk/latency requirements call for it. | Adversarial tests, fuel/time/memory policy, native/WASM equivalence, clear failures. |
| P7 | Improve structured diagnostics and AI repair feedback. Pilot Ariadne only in a host-side presentation surface if it adds value. | Stable machine-readable diagnostics, model repair regression fixtures, UI source ranges. |
| P8 | Expose existing trade/orderbook intelligence through capability-gated APIs; develop missing book metrics only in `analytics-core`. | Required feed granularity, approximate/exact labeling, time alignment, and data-unavailable tests. |
| P9 | Extend chart ABI or add editor/MCP adapters only for validated user needs. | Rust renderer remains authoritative; tool actions retain gateway, replay, and risk permissions. |

## Risks and migration considerations

- **Semantic drift:** popular library names do not guarantee same EMA seed,
  RSI scale, session boundaries, missing-value policy, or warm-up. Pin each
  contract with fixtures.
- **Lookahead/data alignment:** multi-timeframe and trade/book data need
  timestamp and closed-bar rules. A faster kernel that consumes future values
  is incorrect.
- **Resource accounting:** measure inner kernel work, allocations, output
  size, and host data size—not only AST steps. Keep external execution behind
  capability and resource boundaries.
- **Dependency/MSRV growth:** Rhai 1.66, Chumsky 1.65, Logos 1.80 are within
  the current 1.85 floor at inspected versions; Rune/Salsa 1.95, Pest 1.88,
  Tree-sitter 1.90, Wasmtime main 1.97, Wickra 1.86, ndarray 1.87,
  nalgebra/statrs 1.89, RustFFT 1.88, HftBacktest 1.91, and `rmcp` 1.88
  exceed it at their inspected heads. Re-check MSRV at adoption time.
- **Licensing:** NautilusTrader's LGPL and MCP's mixed source/documentation
  terms need elevated review; unverified projects remain excluded. Keep
  notices and transitive dependency audits.
- **Platform duplication:** replacing backtester/charting with external
  projects creates competing models and compatibility burdens. Prefer
  adapters only where a demonstrated boundary is missing.
- **Benchmark interpretation:** project-published figures may compare
  incremental streaming with repeated batch runs. Keep them contextual and
  never present them as QUANTERA's expected gain.

## Research completion and remaining ambiguity

The listed repositories have now been inspected at source level and compared
against QUANTERA; no implementation or dependency change is included in this
report. Two identity limitations remain explicit rather than guessed:

- “TradeRyte Trading Charts”: no verifiable public repository was found.
- “ta-rs”: `greyblake/ta-rs` was inspected as a candidate, not confirmed as the
  user's intended repository.

The next step is to review this blueprint and select Phase 0/P1/P2 evidence
work before any Light Pine architecture or dependency change.
