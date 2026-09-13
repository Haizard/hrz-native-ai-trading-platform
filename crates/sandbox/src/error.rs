//! Errors returned by `sandbox`.

use thiserror::Error;

/// Why a sandboxed execution did not complete normally.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// The module consumed its instruction budget (infinite loop guard).
    #[error("fuel exhausted after {0} instructions")]
    FuelExhausted(u64),

    /// Wall-clock deadline exceeded.
    #[error("execution timed out after {0} ms")]
    Timeout(u64),

    /// The module exceeded its memory limit.
    #[error("memory limit exceeded: {used} bytes used, limit {limit}")]
    MemoryLimitExceeded {
        /// Bytes the module attempted to use.
        used: usize,
        /// Configured ceiling.
        limit: usize,
    },

    /// The module tried to call a host function it was not granted.
    #[error("capability denied: `{0}` is not on the allowlist")]
    CapabilityDenied(String),

    /// The WASM module itself was malformed or failed to instantiate.
    #[error("module instantiation failed: {0}")]
    Instantiation(String),
}
