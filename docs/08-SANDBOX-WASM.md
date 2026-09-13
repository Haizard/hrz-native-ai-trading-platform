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
Both the backtester and the live/paper trading engine call this same function — there is
no separate "trusted" fast path for AI-generated strategies vs. developer-authored ones;
everything not part of the platform's own built-in indicator library goes through the
sandbox.

## Done criteria
- The Phase 3 sample strategy executes identically (same signals) natively via
  `strategy-runtime` and inside the WASM sandbox.
- All adversarial cases above are covered by tests and fail safely (no host crash, no
  resource exhaustion of the host process).
