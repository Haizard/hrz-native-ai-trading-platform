//! # `sandbox`
//!
//! Guarantees that no AI-generated or user-submitted strategy can reach anything
//! beyond market data and simulated order placement: no filesystem, no network,
//! no shell, no secrets, no other strategies' state (principle #6).
//!
//! ## Pipeline
//!
//! ```text
//! natural language -> LLM -> StrategyDocument
//!   -> strategy-dsl validator   (reject early, reject cheaply)
//!   -> sandbox compiler -> WASM module -> sandbox runtime -> execution
//! ```
//!
//! ## The decision this crate is built around
//!
//! We compile the **interpreter** (`strategy-runtime`) to WASM and pass the
//! untrusted strategy document in as *data*. We never compile untrusted code.
//!
//! The alternative -- generating Rust from a strategy and compiling it -- needs a
//! toolchain in the hot path and turns "is this strategy safe" into "is this
//! generated program safe", which is not a question a test suite can answer.
//! Interpreting a closed vocabulary is: the document names fields, functions and
//! comparisons that `strategy-dsl` already type-checked, so the interesting
//! question shrinks to "can a document make the interpreter misbehave", and that
//! one is finite.
//!
//! ## How isolation is enforced
//!
//! Three mechanisms, none of which relies on the guest co-operating:
//!
//! 1. **The import section is checked before instantiation**
//!    ([`allowlist::check_module`]). A module that imports anything outside four
//!    names is refused while it is still bytes, so there is no instance whose
//!    `fd_write` could be reached and no runtime path that could ignore the
//!    refusal.
//! 2. **The host decides what the guest may see.** The guest asks for a
//!    timeframe by name; a name the document did not declare is refused and
//!    recorded. There is no call that returns "everything".
//! 3. **Limits are enforced outside the guest** -- fuel and epoch by wasmtime,
//!    memory by wasmtime's limiter ([`limits::SandboxLimits`]).
//!
//! ## What is *not* claimed
//!
//! wasmtime is a large dependency and this is not a formally verified sandbox.
//! The claim is narrower and testable: a document cannot make the host do
//! anything outside the four host functions, and a document that misbehaves
//! fails with the host process still running. The adversarial suite in
//! `tests/adversarial.rs` is where that claim is checked.
//!
//! ## Example
//!
//! ```
//! use sandbox::{Sandbox, SandboxedStrategy};
//! use strategy_dsl::parse_and_validate;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let document = parse_and_validate(
//!     r#"{
//!         "name": "Example",
//!         "version": "1",
//!         "kind": "strategy",
//!         "market": "BTCUSDT",
//!         "timeframes": {"entry": "5m"},
//!         "entry": {"all_of": [
//!             {"timeframe": "entry", "condition": "close > threshold(0)"}
//!         ]},
//!         "risk": {"max_risk_pct": 1.0, "stop": {"kind": "below_recent_low", "bars": 20}},
//!         "invalidation": [{"timeframe": "entry", "condition": "close_below(vwap)"}]
//!     }"#,
//! )?;
//!
//! let sandbox = Sandbox::new()?;
//! let strategy = SandboxedStrategy::new(&sandbox, &document)?;
//! assert!(strategy.is_clean());
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]

pub mod allowlist;
pub mod error;
pub mod host;
pub mod instance;
pub mod limits;
pub mod strategy;

pub use allowlist::{check_module, is_allowed, ALLOWED_HOST_FUNCTIONS, ALLOWED_IMPORT_MODULE};
pub use error::SandboxError;
pub use host::HostState;
pub use instance::{
    engine_config, guest_wasm, ResourceUsage, Sandbox, SandboxExecutionResult, Session, ABI_VERSION,
};
pub use limits::SandboxLimits;
pub use strategy::SandboxedStrategy;

/// The types most callers need.
pub mod prelude {
    pub use crate::error::SandboxError;
    pub use crate::instance::{ResourceUsage, Sandbox, SandboxExecutionResult, Session};
    pub use crate::limits::SandboxLimits;
    pub use crate::strategy::SandboxedStrategy;
}
