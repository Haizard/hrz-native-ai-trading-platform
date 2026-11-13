# 08 — Sandbox (Safe Execution of AI-Generated Strategies)

## Purpose
Ensure that no AI-generated (or user-submitted) strategy/indicator can access anything
beyond market data and simulated order placement — no filesystem, no network, no shell,
no secrets, no host process memory beyond its own sandbox.

## Pipeline (per principle #6)
```
Natural language → LLM → StrategyDocument → strategy-dsl validator
   → sandbox compiler → WASM module → sandbox runtime → execution → results
```
The validator (see `docs/06-STRATEGY-DSL.md`) runs **before** anything reaches the
sandbox — reject early, reject cheaply.

## Runtime choice
Use a WASM runtime with strong sandboxing and resource-limiting support (e.g.
`wasmtime`). Compile the interpreted `strategy-runtime` execution path (not raw
LLM-authored Rust) to WASM, and pass the validated `StrategyDocument` in as data — the
sandbox is executing the **interpreter**, with the untrusted strategy as its input, not
compiling untrusted code to native instructions. This avoids needing a full Rust
compiler in the hot path and keeps the attack surface to "can this document cause the
interpreter to do something unsafe," which is a much smaller and testable question.

## Capability allowlist
```
ALLOW:
  - read: current MarketState for declared timeframes
  - read: strategy's own persisted state (scoped key-value)
  - write: emit Signal / simulated order intents
  - read: wall-clock time (for session-boundary logic only)

DENY:
  - filesystem access
  - network access
  - process/thread spawning
  - environment variables
  - access to other strategies' state
  - access to secrets/credentials
```
Enforce this at the WASM host-function boundary — the sandboxed module can only call
the specific host functions exposed to it (`get_market_state`, `get_state`, `set_state`,
`emit_signal`), nothing else is linked in.

## Resource limits
- Wall-clock execution timeout per `on_candle` invocation (e.g. low milliseconds).
- Memory limit per instance.
- Fuel/instruction-count limit (wasmtime's fuel metering) to stop infinite loops
  deterministically rather than relying on wall-clock alone.

## Adversarial test suite (required before Phase 4 exit)
- Infinite loop in a condition evaluator → must be halted by fuel limit, not hang the
  host.
- Extremely large `timeframes`/condition list designed to exhaust memory → must be
  rejected by the validator's size limits before reaching the sandbox.
- Attempted access to a non-allowlisted host function → compile/link failure, not a
  runtime panic that could be caught and ignored.
- Malformed numeric values (NaN/Infinity injected into a condition) → defined,
  tested behavior (reject or treat as false), never propagate into order sizing.

## Interface with the rest of the system
```rust
pub struct SandboxExecutionResult {
    pub signals: Vec<Signal>,
    pub errors: Vec<SandboxError>,
    pub resource_usage: ResourceUsage,
}

pub fn execute_in_sandbox(
    doc: &StrategyDocument,
    market_context: &MarketContext,
) -> SandboxExecutionResult;
```
The live/paper trading engine calls this path — there is no separate "trusted" fast path
for AI-generated strategies vs. developer-authored ones. The backtester does not depend on
`sandbox` today: it drives `strategy-runtime` natively, and the equivalence test in
`crates/sandbox/tests/equivalence.rs` proves the two paths agree. A backtester that also
runs through the sandbox would be a smaller change than this one was, because the seam
(`SandboxedStrategy` implementing `Strategy`) already exists.

## Done criteria
- The Phase 3 sample strategy executes identically (same signals) natively via
  `strategy-runtime` and inside the WASM sandbox.
- All adversarial cases above are covered by tests and fail safely (no host crash, no
  resource exhaustion of the host process).

---

## Pine-Lite preview boundary (decision, 2026-11)

The gateway's indicator preview currently calls `pine_lite::vet` and
`pine_lite::interp::run` in the gateway process. Static script limits and the
interpreter fuel meter bound ordinary script work, but they do not impose a
memory ceiling or isolate a native interpreter/runtime fault from the service.
This path is not the Wasmtime sandbox and must not be described as isolated.

**Decision:** keep Pine-Lite limited to producing chart-preview results, with no
live execution, broker, or order-placement capability. The gateway now runs
the trusted Pine-Lite interpreter as WebAssembly inside the existing Wasmtime
host (`sandbox-guest::pine`), passing source, candles, and provider-backed
series as bounded data. The guest receives no filesystem, network, environment,
secret, broker, or order-placement capability; preview data is fetched by the
host and the guest only evaluates that snapshot.

The gateway preflights no more than 50,000 chart candles, 100,000 total candle
and provider values, eight secondary market pools, 64 provider series, and
16 MiB of serialized input. The guest response is capped at 4 MiB; Wasmtime's
default 64 MiB linear-memory limit, 100,000,000 fuel, and 250 ms epoch
deadline apply. A gateway semaphore allows four active Pine preview jobs and
rejects excess work instead of queueing it without bound. The guest returns
reduced chart evidence, not its full per-bar plot buffers.

The Wasmtime guest is an in-process WebAssembly boundary: its linear memory and
imports are constrained, but it does not contain a Wasmtime or host integration
vulnerability from the gateway's OS process. On Linux, server-side Pine-Lite
previews therefore run in the packaged `pine-preview-worker` child executable.
The gateway sends bounded length-prefixed JSON over stdin/stdout, clears each
worker's environment, enforces per-request timeouts, and requires the worker's
startup self-test to prove filesystem and network denial before serving.
The worker installs Landlock and seccomp restrictions, disallows process
creation and network syscalls, applies CPU/address-space/file/descriptor limits,
handles at most 32 requests before recycling, and still applies Wasmtime's
guest-level fuel, epoch, memory, and import limits.
Landlock is installed before Wasmtime initialization so its epoch-ticker thread
inherits filesystem denial; seccomp is synchronized to every worker thread.
Missing kernel support or an unavailable worker is a startup error; Linux does
not fall back to in-process preview execution. Non-Linux development currently
uses the explicitly in-process Wasmtime backend.

The guest integration is covered by `crates/sandbox/tests/pine_preview.rs` and
`crates/sandbox-guest/src/pine.rs`: native-versus-guest evidence parity,
invalid-source refusal, fuel exhaustion, input caps, module import allowlisting,
and bounded output conversion. `cargo check -p pine-lite --target
wasm32-unknown-unknown` preserves the browser build. These checks do not claim
process isolation by themselves; the Linux child-process boundary and its
kernel-policy probes are covered by the gateway worker integration tests and
must pass on the deployment kernel.

Existing evidence is deliberately kept separate: Pine-Lite's runaway-loop
tests prove its interpreter fuel guard; `sandbox/tests/adversarial.rs` tests
Wasmtime's import allowlist, fuel, memory, and trap handling for the
strategy-runtime guest. Neither suite proves Pine-Lite is already sandboxed.
The Wasmtime timeout also has a dedicated epoch-interruption test.

---

# Implementation notes (Phase 4)

*The section above is the contract. This section records what was built against it, the
decisions that were not obvious from the contract, and the four bugs that only running
the thing could have found. It is written for whoever changes this code next.*

## Where it lives

| Crate | Role |
|---|---|
| `crates/sandbox-guest` | The interpreter (`strategy-runtime`) plus a thin `extern "C"` ABI, compiled to `wasm32-unknown-unknown`. Also builds as an `rlib` so its portable core (`interpret`) is unit-testable on the host. |
| `crates/sandbox` | The wasmtime host: import allowlist, limits, host functions, `SandboxedStrategy`. |
| `crates/sandbox/build.rs` | Compiles the guest and embeds the module. |

The guest is built by `sandbox`'s build script rather than committed as a `.wasm`, so the
module inside the host binary is always the one this commit's source produces — there is
no artifact to forget to regenerate. The nested `cargo build` gets its **own**
`--target-dir`: Cargo holds an exclusive lock on the target directory for the duration of
a build, so a build script building into the same one would deadlock against its own
parent.

## The ABI

Seven exports (`sbx_abi_version`, `sbx_alloc`, `sbx_free`, `sbx_init`, `sbx_eval`,
`sbx_error_ptr`, `sbx_error_len`), two imports, and one exported memory.

The imports are **`host_market_state`**, **`host_emit_signal`**, **`host_state_get`** and
**`host_state_set`** — the spec's four capabilities, prefixed. The `host_` prefix is not
decoration: it makes an import line in a module's import section self-evidently a host
call, so a reviewer reading raw WASM sees the boundary without needing this document.
`host_state_get`/`host_state_set` are linked but the shipped interpreter does not call
them yet; they are part of the ABI so a strategy needing persistence does not force a
protocol bump.

Everything crosses the boundary as JSON through the guest's **own** allocator: the host
calls the guest's `sbx_alloc` and writes into the returned region, so the guest's heap
stays owned by the guest's allocator. Nothing is ever read from a pointer the host
guessed at.

## Where each attack stops

The interesting property is that the four mechanisms stop four *different* things, and
none of them is a backstop for another:

| Attack | Stopped by | Where |
|---|---|---|
| Import something forbidden | import-section scan of the `Module` | **before instantiation** — no instance exists whose forbidden function could be reached, so the refusal is not a runtime condition that could be caught |
| Run forever | fuel metering | deterministic instruction budget, primary |
| Run too long while consuming little fuel | epoch interruption | wall-clock backstop; a ticker thread increments the epoch every 1 ms |
| Consume host memory | wasmtime `ResourceLimiter` | per-instance, enforced outside the guest |

Fuel is the primary infinite-loop guard because it is **deterministic** — the same
document burns the same fuel on every machine, so a limit that passes in CI passes in
production. The wall-clock timeout exists only for a guest that stalls without burning
fuel, which fuel metering cannot see.

Limits are per `on_candle` invocation and are re-armed before **every** call into the
guest, including `sbx_init`. This is not tidiness: a wasmtime store starts with *no* fuel,
so a call made before arming traps with `OutOfFuel` on its first instruction. That is how
the first working build reported an exhausted budget for a document it had not yet read.

## The three limits

`SandboxLimits` (in `crates/sandbox/src/limits.rs`):

| Limit | Default | Rationale |
|---|---|---|
| `fuel` | 100,000,000 | ~1000x what the sample strategy needs per candle |
| `timeout` | 250 ms | backstop only; fuel normally fires first |
| `max_memory_bytes` | 64 MiB | the interpreter's working set is kilobytes |
| `max_signals` | 4 | a strategy emitting more than a few entries per candle is a bug |
| `max_state_entries` / `max_state_bytes` | 64 / 4096 | bounds the scoped store |
| `max_message_bytes` | 64 KiB | bounds every JSON message in either direction |

## What the equivalence tests forced

**`serde_json`'s `float_roundtrip` feature is load-bearing.** The default float parser is
best-effort rather than correctly rounded, so for some inputs it is one ULP off.
`103.2` goes out, `103.19999999999999` comes back. That is invisible until you compare two
runs that differ only in whether a number crossed a JSON boundary — which is what the
sandbox does on every candle. The symptom was native-vs-sandboxed backtests disagreeing in
the last decimal of nearly every trade, with correct-looking logic on both sides. No
amount of care inside the sandbox could have fixed it; the fix is a workspace-level
feature flag. The cost is roughly 2x on float parsing, which is not the bottleneck
anywhere in this system.

**Warm-up is not the same as "not declared."** A 1h context timeframe has nothing to say
during the first 48 five-minute bars. The native path treats a timeframe with no closed
candle as absent, and the first version of the host treated it as an error — so every
candle failed with `the host declined the context view`. The boundary now has three
outcomes rather than two, and the distinction is carried by the host, which knows the
document's declared set:

```
Ok(Some(view))  -> here it is
Ok(None)        -> declared, but no candle has closed yet (warm-up)
Err(_)          -> the host refused: undeclared, or a protocol violation
```

The third case is the one worth keeping. Collapsing it into `Ok(None)` would make "your
document names a timeframe that does not exist" indistinguishable from "wait two days".

## The YAML feature gate

YAML is **off by default across the workspace** and opted into by the crates that need it
(the CLI, and test fixtures). This is a security property, not tidiness: Cargo unifies
features across a dependency graph, so a single crate on the guest's path enabling
`strategy-dsl/yaml` would link a YAML parser into the sandbox even though the guest only
ever reads JSON.

Two things about this are easy to get wrong:

- **`default-features = false` only works in the `[workspace.dependencies]` entry.** On an
  inherited dependency (`strategy-dsl = { workspace = true, default-features = false }`)
  Cargo ignores it with a warning and quietly enables the default. The gate has to be
  expressed once, at the root.
- **Nothing fails when it regresses.** Feature unification is silent. CI therefore reads
  the *resolved* graph (`cargo tree -p sandbox-guest --target wasm32-unknown-unknown`)
  rather than the manifests, and `build.rs` carries a canary that fails the build if the
  compiled module contains `unsafe-libyaml` or `serde_yaml`.

## How equivalence is actually demonstrated

`SandboxedStrategy` implements `strategy_runtime::Strategy`. The backtester's own `replay`
loop drives it unchanged, so the claim under test is "one loop, driven two ways" rather
than "two implementations that agree" — there is no second replay loop to drift.

`crates/sandbox/tests/equivalence.rs` runs three synthetic multi-timeframe documents and
both shipped Phase 3 documents, asserting the `ReplayOutput` is **byte-identical**. Each
synthetic document first asserts that it actually traded: an earlier version of the
fixture climbed monotonically, so `swing_highs` was empty, every document was a no-op, and
the tests passed while proving nothing. `the_fixture_contains_the_setups_it_claims_to`
now guards the fixture itself.

`crates/sandbox/tests/adversarial.rs` covers the four cases this document requires plus
the malformed-module and pre-sandbox rejection paths — 22 tests. The hostile modules are
hand-written WAT, not Rust: the shapes an attacker sends (an endless loop, a memory bomb,
`unreachable`, a module importing WASI) are shapes our own compiler does not emit, so
testing against Rust output would test the wrong thing.

## What is *not* claimed

- **Not a security boundary against a hostile host.** The threat model is a hostile
  *document* and, later, a hostile *guest*. wasmtime's correctness is assumed.
- **Not a defence against side channels.** Fuel consumption and timing vary with the
  input; nothing here hides that.
- **Not a proof of the interpreter's semantics.** Equivalence says the sandbox runs the
  same interpreter as the native path. It says nothing about whether the interpreter is
  right.
- **Not yet wired into the trading engine.** `trading-engine` is the caller that will use
  this in Phase 6; today the backtester is.

