//! # `sandbox`
//!
//! Guarantees that no AI-generated or user-submitted strategy can reach
//! anything beyond market data and simulated order placement: no filesystem,
//! no network, no shell, no secrets, no other strategies' state (principle #6).
//!
//! ## Pipeline
//!
//! ```text
//! natural language -> LLM -> StrategyDocument
//!   -> strategy-dsl validator   (reject early, reject cheaply)
//!   -> sandbox compiler -> WASM module -> sandbox runtime -> execution
//! ```
//!
//! ## Key design decision (`docs/08-SANDBOX-WASM.md`)
//!
//! We compile the **interpreter** (`strategy-runtime`) to WASM and pass the
//! untrusted strategy document in as *data*. We never compile untrusted code.
//! That shrinks the attack surface to "can this document make the interpreter
//! misbehave", which is small and testable, and avoids a Rust compiler in the
//! hot path.
//!
//! Capabilities are enforced at the WASM host-function boundary: only
//! `get_market_state`, `get_state`, `set_state` and `emit_signal` are linked.
//!
//! ## Status
//!
//! Phase 0 skeleton. Phase 4 adds `wasmtime`, fuel metering, memory/time
//! limits and the adversarial test suite.

#![deny(missing_docs)]

pub mod error;

pub use error::SandboxError;
