//! The host side of the sandbox boundary: the four allowlisted functions, and
//! the state they act on.
//!
//! ## The shape of the boundary
//!
//! The host decides what a guest may see, and the guest asks for it by name.
//! That ordering matters. If the host pushed the whole `MarketContext` into the
//! call, a document that referenced a timeframe nobody declared would simply
//! find nothing there; here it is refused, recorded, and visible in
//! [`SandboxExecutionResult::errors`](crate::SandboxExecutionResult).
//!
//! ## Return codes
//!
//! Every function returns `i32`. Non-negative is a pointer into guest memory (or
//! `0` for "nothing there"); negative is a refusal. The guest treats any
//! negative as a failure and stops, so a refusal cannot be ignored -- it turns
//! into a `-1` from `sbx_eval`, and the host reads the reason back.
//!
//! ## What is deliberately not here
//!
//! No WASI. No clock, no filesystem, no randomness, no environment. The spec's
//! allowlist mentions wall-clock time "for session-boundary logic only", and it
//! is not exposed: nothing in the DSL reads a clock, and a capability that no
//! caller needs is a capability that should not exist.

use std::collections::{BTreeMap, BTreeSet};

use strategy_runtime::context::MarketContext;
use strategy_runtime::signal::Signal;
use wasmtime::{Caller, Extern, Memory, StoreLimits, Val};

use crate::limits::SandboxLimits;

/// The call succeeded; the return value is a pointer or `0`.
const OK: i32 = 0;
/// The guest asked for something the host will not give it.
const DENIED: i32 = -1;
/// The host was not ready to answer.
const NOT_READY: i32 = -2;
/// The guest exceeded a channel's ceiling.
const TOO_MANY: i32 = -3;
/// The guest sent something the host could not read.
const BAD_INPUT: i32 = -4;
/// The host could not allocate or write in guest memory.
const OUT_OF_MEMORY: i32 = -5;

/// Everything a guest instance can reach.
pub struct HostState {
    /// The timeframe names the document declared.
    ///
    /// Kept separately from the context's own map, because that map only holds
    /// timeframes that have a *closed candle*. Without this set the host could
    /// not tell "you never declared this" from "you declared it and it has
    /// nothing to say yet", and the second is ordinary warm-up -- a 4h context
    /// timeframe is silent for the first 48 bars of a 5m series.
    declared: BTreeSet<String>,
    /// What the guest is allowed to see this evaluation.
    context: Option<MarketContext>,
    /// Signals the guest emitted, in order.
    signals: Vec<Signal>,
    /// The strategy's own scoped key-value store.
    state: BTreeMap<String, String>,
    /// Ceilings for this instance.
    limits: SandboxLimits,
    /// Handed to wasmtime so it can refuse memory growth itself.
    store_limits: StoreLimits,
    /// Requests the host refused, in the guest's own words.
    denials: Vec<String>,
    /// Writes the host refused for exceeding a ceiling.
    refusals: Vec<String>,
}

/// How many refusals are worth remembering.
///
/// A guest that asks for the same forbidden thing on every candle would
/// otherwise grow these logs without bound -- the log has to be bounded for the
/// same reason the sandbox exists.
const MAX_LOGGED_REFUSALS: usize = 64;

impl HostState {
    /// A fresh state with nothing to show the guest yet.
    #[must_use]
    pub fn new(limits: SandboxLimits) -> Self {
        Self {
            declared: BTreeSet::new(),
            context: None,
            signals: Vec::new(),
            state: BTreeMap::new(),
            limits,
            store_limits: wasmtime::StoreLimitsBuilder::new()
                .memory_size(limits.max_memory_bytes)
                .build(),
            denials: Vec::new(),
            refusals: Vec::new(),
        }
    }

    /// Record which timeframe names the document declared.
    pub fn declare(&mut self, names: impl IntoIterator<Item = String>) {
        self.declared = names.into_iter().collect();
    }

    /// Record a refusal, keeping the log bounded.
    fn note(&mut self, entry: String) {
        if self.refusals.len() < MAX_LOGGED_REFUSALS {
            self.refusals.push(entry);
        }
    }

    /// Publish a context for the next evaluation, clearing the last one's output.
    pub fn begin(&mut self, context: MarketContext) {
        self.context = Some(context);
        self.signals.clear();
        self.denials.clear();
        self.refusals.clear();
    }

    /// Take the signals the guest emitted.
    pub fn take_signals(&mut self) -> Vec<Signal> {
        std::mem::take(&mut self.signals)
    }

    /// Requests the host refused.
    #[must_use]
    pub fn denials(&self) -> &[String] {
        &self.denials
    }

    /// Writes the host refused for exceeding a ceiling.
    #[must_use]
    pub fn refusals(&self) -> &[String] {
        &self.refusals
    }

    /// The wasmtime limiter, which is how the memory ceiling is actually
    /// enforced: wasmtime asks this before it lets the guest grow memory, so the
    /// ceiling does not depend on the guest co-operating.
    pub fn limiter(&mut self) -> &mut dyn wasmtime::ResourceLimiter {
        &mut self.store_limits
    }

    /// The scoped key-value store, for tests and diagnostics.
    #[must_use]
    pub fn scoped_state(&self) -> &BTreeMap<String, String> {
        &self.state
    }
}

/// Hand a signal out of the sandbox.
fn host_emit_signal(mut caller: Caller<'_, HostState>, ptr: u32, len: u32) -> i32 {
    let Some(memory) = memory_of(&mut caller) else {
        return NOT_READY;
    };
    let Some(raw) = read(&caller, &memory, ptr, len) else {
        return BAD_INPUT;
    };
    let Ok(signal) = serde_json::from_slice::<Signal>(&raw) else {
        return BAD_INPUT;
    };

    if caller.data().signals.len() >= caller.data().limits.max_signals {
        let limit = caller.data().limits.max_signals;
        caller
            .data_mut()
            .note(format!("more than {limit} signals in one evaluation"));
        return TOO_MANY;
    }

    caller.data_mut().signals.push(signal);
    OK
}

/// Give the guest one declared timeframe's view.
///
/// Three outcomes, and the middle one is why this is not a two-way decision:
///
/// * a pointer -- the view, as JSON.
/// * `0` -- the name is declared but no candle of it has closed yet. Warm-up,
///   which the native path also treats as "condition is absent", not as an
///   error.
/// * `DENIED` -- the name was never declared. This is the capability check, and
///   it is what stops a document reaching for a timeframe nobody gave it.
fn host_market_state(mut caller: Caller<'_, HostState>, name_ptr: u32, name_len: u32) -> i32 {
    let Some(memory) = memory_of(&mut caller) else {
        return NOT_READY;
    };
    let Some(raw) = read(&caller, &memory, name_ptr, name_len) else {
        return BAD_INPUT;
    };
    let Ok(name) = String::from_utf8(raw) else {
        return BAD_INPUT;
    };

    if !caller.data().declared.contains(&name) {
        tracing::warn!(timeframe = %name, "sandbox denied a market-state read");
        let entry = format!("market state for `{name}`");
        if caller.data().denials.len() < MAX_LOGGED_REFUSALS {
            caller.data_mut().denials.push(entry.clone());
        }
        caller.data_mut().note(entry);
        return DENIED;
    }

    // Cloned so the immutable borrow of `caller` ends before the mutable one.
    let view = caller
        .data()
        .context
        .as_ref()
        .and_then(|context| context.timeframes.get(&name))
        .cloned();

    let Some(view) = view else {
        return OK;
    };

    let Ok(json) = serde_json::to_vec(&view) else {
        return BAD_INPUT;
    };
    write_framed(&mut caller, &memory, &json)
}

/// Read one key from the guest's scoped store.
fn host_state_get(mut caller: Caller<'_, HostState>, key_ptr: u32, key_len: u32) -> i32 {
    let Some(memory) = memory_of(&mut caller) else {
        return NOT_READY;
    };
    let Some(raw) = read(&caller, &memory, key_ptr, key_len) else {
        return BAD_INPUT;
    };
    let Ok(key) = String::from_utf8(raw) else {
        return BAD_INPUT;
    };

    let value = caller.data().state.get(&key).cloned();
    match value {
        // `0` is "no such key", which is distinct from every valid pointer
        // because `sbx_alloc` never returns 0 for a successful allocation.
        None => OK,
        Some(value) => write_framed(&mut caller, &memory, value.as_bytes()),
    }
}

/// Write one key into the guest's scoped store.
fn host_state_set(
    mut caller: Caller<'_, HostState>,
    key_ptr: u32,
    key_len: u32,
    val_ptr: u32,
    val_len: u32,
) -> i32 {
    let Some(memory) = memory_of(&mut caller) else {
        return NOT_READY;
    };
    let Some(raw_key) = read(&caller, &memory, key_ptr, key_len) else {
        return BAD_INPUT;
    };
    let Some(raw_value) = read(&caller, &memory, val_ptr, val_len) else {
        return BAD_INPUT;
    };
    let (Ok(key), Ok(value)) = (String::from_utf8(raw_key), String::from_utf8(raw_value)) else {
        return BAD_INPUT;
    };

    let limits = caller.data().limits;
    if value.len() > limits.max_state_bytes {
        let actual = value.len();
        let ceiling = limits.max_state_bytes;
        caller
            .data_mut()
            .note(format!("state value of {actual} bytes exceeds {ceiling}"));
        return TOO_MANY;
    }

    let state = &mut caller.data_mut().state;
    if !state.contains_key(&key) && state.len() >= limits.max_state_entries {
        let ceiling = limits.max_state_entries;
        caller
            .data_mut()
            .note(format!("more than {ceiling} scoped state entries"));
        return TOO_MANY;
    }
    state.insert(key, value);
    OK
}

/// Register the four host functions. Nothing else is linked, so nothing else
/// can be reached even if a module somehow imported it.
pub fn link(linker: &mut wasmtime::Linker<HostState>) -> Result<(), wasmtime::Error> {
    linker.func_wrap("env", "host_emit_signal", host_emit_signal)?;
    linker.func_wrap("env", "host_market_state", host_market_state)?;
    linker.func_wrap("env", "host_state_get", host_state_get)?;
    linker.func_wrap("env", "host_state_set", host_state_set)?;
    Ok(())
}

/// The guest's exported linear memory.
fn memory_of(caller: &mut Caller<'_, HostState>) -> Option<Memory> {
    match caller.get_export("memory")? {
        Extern::Memory(memory) => Some(memory),
        _ => None,
    }
}

/// Copy `ptr..ptr + len` out of guest memory.
///
/// Bounds-checked rather than trusted: a guest that hands over a bad pointer
/// gets `None`, not a host-side read of whatever happens to be there.
fn read(caller: &Caller<'_, HostState>, memory: &Memory, ptr: u32, len: u32) -> Option<Vec<u8>> {
    let data = memory.data(caller);
    let start = usize::try_from(ptr).ok()?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    data.get(start..end).map(<[u8]>::to_vec)
}

/// Allocate in the guest and write `[u32 length][payload]` there.
///
/// The length prefix is what lets the guest find the end of the payload without
/// a second call. Returns the pointer, or a negative code.
fn write_framed(caller: &mut Caller<'_, HostState>, memory: &Memory, payload: &[u8]) -> i32 {
    let Ok(payload_len) = u32::try_from(payload.len()) else {
        return OUT_OF_MEMORY;
    };
    let Ok(total) = u32::try_from(payload.len() + 4) else {
        return OUT_OF_MEMORY;
    };

    let Some(ptr) = guest_alloc(caller, total) else {
        return OUT_OF_MEMORY;
    };

    let mut framed = Vec::with_capacity(payload.len() + 4);
    framed.extend_from_slice(&payload_len.to_le_bytes());
    framed.extend_from_slice(payload);

    if memory.write(&mut *caller, ptr as usize, &framed).is_err() {
        return OUT_OF_MEMORY;
    }
    ptr as i32
}

/// Call the guest's own allocator, so memory it will free belongs to it.
///
/// Borrowing the guest's allocator rather than writing at a fixed offset means
/// the guest's Rust allocator stays the single owner of its heap: the sandbox
/// never hands the guest a pointer the guest did not produce.
fn guest_alloc(caller: &mut Caller<'_, HostState>, len: u32) -> Option<u32> {
    let func = match caller.get_export("sbx_alloc")? {
        Extern::Func(func) => func,
        _ => return None,
    };
    let mut results = [Val::I32(0)];
    // Signed `i32` is the wire representation; the value is an unsigned offset
    // in a memory far smaller than `i32::MAX`.
    func.call(&mut *caller, &[Val::I32(len as i32)], &mut results)
        .ok()?;
    match results[0] {
        Val::I32(0) => None,
        Val::I32(ptr) => Some(ptr as u32),
        _ => None,
    }
}
