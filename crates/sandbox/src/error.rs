//! Errors returned by `sandbox`.
//!
//! Every variant is a *safe* failure: the host process is still running and
//! still holds a usable sandbox when one of these comes back. That is the whole
//! point of the crate -- `docs/08-SANDBOX-WASM.md` requires that a hostile
//! strategy be rejected rather than crash the thing executing it.

use std::time::Duration;

use thiserror::Error;

/// Why a sandboxed execution did not complete normally.
#[derive(Debug, Clone, Error)]
pub enum SandboxError {
    /// The module imported something the allowlist does not grant.
    ///
    /// Raised before instantiation, from the module's import section, so the
    /// capability is never linked in the first place. There is no runtime path
    /// that can catch this and carry on.
    #[error("capability denied: `{0}` is not on the sandbox allowlist")]
    CapabilityDenied(String),

    /// The module could not be read, checked or instantiated.
    #[error("module could not be instantiated: {0}")]
    Instantiation(String),

    /// The module does not speak the ABI this host does.
    #[error("the module implements ABI version {found}, this host speaks {expected}")]
    AbiMismatch {
        /// Version the module declares.
        found: u32,
        /// Version this build of `sandbox` speaks.
        expected: u32,
    },

    /// The module does not export something the host must call.
    #[error("the module does not export `{0}`")]
    MissingExport(String),

    /// The module consumed its whole instruction budget.
    ///
    /// This is the deterministic guard against an infinite loop: it fires after
    /// a fixed number of instructions regardless of how fast the machine is.
    #[error("fuel exhausted after {used} instructions (budget {budget})")]
    FuelExhausted {
        /// Instructions actually consumed.
        used: u64,
        /// Instructions the instance was granted.
        budget: u64,
    },

    /// Wall-clock deadline exceeded.
    #[error("execution exceeded its {0:?} deadline")]
    Timeout(Duration),

    /// The module asked for more linear memory than it is allowed.
    #[error("memory limit exceeded: the module wanted more than {limit} bytes")]
    MemoryLimitExceeded {
        /// Configured ceiling.
        limit: usize,
    },

    /// The module trapped -- a panic, an `unreachable`, a bad memory access.
    ///
    /// The guest is compiled with `panic = "abort"`, so a Rust panic inside the
    /// sandbox arrives here as a trap rather than unwinding across the FFI
    /// boundary.
    #[error("the guest trapped: {0}")]
    Trap(String),

    /// The guest rejected the work and explained why.
    #[error("the guest refused the work: {0}")]
    Guest(String),

    /// The guest emitted more signals than one candle may produce.
    #[error("the guest emitted {count} signals, more than the {limit} it is allowed")]
    TooManySignals {
        /// Signals the guest tried to emit.
        count: usize,
        /// Configured ceiling.
        limit: usize,
    },

    /// The guest tried to write more persistent state than it is allowed.
    #[error("the guest's scoped state exceeded its limit: {0}")]
    StateLimitExceeded(String),

    /// The host was asked to do something before it had what it needs.
    #[error("the sandbox is not ready: {0}")]
    NotReady(String),
}
