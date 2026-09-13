//! Resource ceilings for one sandboxed instance.
//!
//! Two of these are load-bearing rather than defensive:
//!
//! * **Fuel** is what makes an infinite loop a *deterministic* failure. A
//!   wall-clock deadline alone would let the same document pass on a fast
//!   machine and fail on a slow one, which is not a property a trading system
//!   can have.
//! * **Memory** is the only thing standing between a document that asks for a
//!   huge window and the host's own address space. `strategy-dsl` caps the
//!   document's size, but not what the interpreter does with it.
//!
//! The rest bound the channels the guest writes through, so that a module which
//! ignores the protocol cannot use them as an unbounded sink.

use std::time::Duration;

/// What one sandboxed instance is allowed to consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxLimits {
    /// Instruction budget for a single evaluation.
    ///
    /// Generous on purpose: deserializing a 500-candle view costs real
    /// instructions, and a legitimate evaluation must never come close. An
    /// infinite loop still exhausts it in a fraction of a second.
    pub fuel: u64,

    /// Wall-clock ceiling for a single evaluation.
    ///
    /// A backstop for the case fuel cannot catch -- a host function that blocks,
    /// say -- rather than the primary guard.
    pub timeout: Duration,

    /// Linear-memory ceiling, in bytes.
    pub max_memory_bytes: usize,

    /// Signals one evaluation may emit.
    ///
    /// The interpreter emits at most one per candle. The cap exists so a module
    /// that does not follow the protocol cannot flood the host.
    pub max_signals: usize,

    /// Entries the guest's scoped key-value store may hold.
    pub max_state_entries: usize,

    /// Bytes one scoped state value may occupy.
    pub max_state_bytes: usize,

    /// Bytes an error message read back from the guest may occupy.
    pub max_message_bytes: usize,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            fuel: 100_000_000,
            timeout: Duration::from_millis(250),
            max_memory_bytes: 64 * 1024 * 1024,
            max_signals: 4,
            max_state_entries: 64,
            max_state_bytes: 4096,
            max_message_bytes: 64 * 1024,
        }
    }
}

impl SandboxLimits {
    /// The same limits with a different instruction budget.
    #[must_use]
    pub const fn with_fuel(mut self, fuel: u64) -> Self {
        self.fuel = fuel;
        self
    }

    /// The same limits with a different wall-clock ceiling.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The same limits with a different memory ceiling.
    #[must_use]
    pub const fn with_memory(mut self, bytes: usize) -> Self {
        self.max_memory_bytes = bytes;
        self
    }
}
