//! The `extern "C"` ABI the sandbox host speaks.
//!
//! Compiled only for `wasm32`. Everything here is pointer marshalling; the
//! decisions live in [`crate::interpret`].
//!
//! ## Protocol
//!
//! ```text
//!   sbx_alloc(len) -> ptr          host reserves guest memory to write into
//!   sbx_init(ptr, len) -> i32      load a document;  0 ok, -1 error
//!   sbx_eval(ptr, len) -> i32      evaluate one candle; 0 ok, -1 error
//!   sbx_error_ptr()/sbx_error_len()  the message for the last -1
//!   sbx_abi_version() -> u32       so the host can refuse a module it does not know
//! ```
//!
//! ## Imports
//!
//! Exactly two, and both are on the sandbox allowlist:
//!
//! * `host_emit_signal` -- hand a signal out. The guest never returns signals
//!   through a buffer, because the host has to count and validate them anyway
//!   and a host function is where that belongs.
//! * `host_market_state` -- ask for one declared timeframe's view.
//!
//! `host_state_get` / `host_state_set` are also allowlisted, and the host links
//! them, but this build does not import them: the DSL grammar has no construct
//! that reads or writes persistent state yet. Importing a capability nothing
//! uses would be the opposite of least authority, so we do not -- and the host
//! links them anyway so that adding one later is not a boundary change.

use std::alloc::{alloc, dealloc, Layout};
use std::cell::RefCell;

use strategy_runtime::context::TimeframeView;

use crate::interpret::{Header, Interpreter, MarketSource};

/// Version of this ABI. Bump when the protocol changes incompatibly.
const ABI_VERSION: u32 = 1;

// `#[link(wasm_import_module = "env")]` is what makes these *imports* rather
// than undefined symbols: without it `rust-lld` refuses to link, because a
// `wasm32-unknown-unknown` module has no libc to resolve them against. The
// module name lands in the import section, which is exactly what the host
// checks against its allowlist before instantiating anything.
#[link(wasm_import_module = "env")]
extern "C" {
    /// Hand one serialized signal to the host. Returns 0 when accepted.
    fn host_emit_signal(ptr: u32, len: u32) -> i32;

    /// Ask the host for a declared timeframe's view.
    ///
    /// Returns a pointer to `[u32 little-endian length][length bytes of JSON]`,
    /// allocated with [`sbx_alloc`] and freed by the caller, or a negative code
    /// when the host declines.
    fn host_market_state(name_ptr: u32, name_len: u32) -> i32;
}

/// The guest's whole mutable world.
struct Guest {
    interpreter: Interpreter,
    /// The message behind the most recent `-1`.
    error: Vec<u8>,
}

thread_local! {
    // WASM is single-threaded, so this is a plain cell with a `RefCell` for
    // interior mutability rather than anything that has to be synchronized.
    static GUEST: RefCell<Guest> = RefCell::new(Guest {
        interpreter: Interpreter::new(),
        error: Vec::new(),
    });
}

impl Guest {
    /// Record `message` and return the error code.
    fn fail(&mut self, message: impl Into<String>) -> i32 {
        self.error = message.into().into_bytes();
        -1
    }
}

/// Borrow a slice of the module's linear memory.
///
/// # Safety
///
/// `ptr..ptr + len` must be entirely inside the module's linear memory, which
/// only the host can guarantee -- the host is the one that allocated it with
/// [`sbx_alloc`] and wrote the bytes.
unsafe fn bytes<'a>(ptr: u32, len: u32) -> &'a [u8] {
    if ptr == 0 || len == 0 {
        return &[];
    }
    std::slice::from_raw_parts(ptr as *const u8, len as usize)
}

/// The ABI version this module implements.
#[no_mangle]
pub extern "C" fn sbx_abi_version() -> u32 {
    ABI_VERSION
}

/// Reserve `len` bytes of linear memory for the host to write into.
///
/// Returns 0 on failure, which is never a valid allocation, so the host can
/// treat it as an error without a separate channel.
#[no_mangle]
pub extern "C" fn sbx_alloc(len: u32) -> u32 {
    if len == 0 {
        return 0;
    }
    let Ok(layout) = Layout::from_size_align(len as usize, 1) else {
        return 0;
    };
    // SAFETY: `layout` has a non-zero size, which is all `alloc` requires.
    let ptr = unsafe { alloc(layout) };
    if ptr.is_null() {
        0
    } else {
        ptr as u32
    }
}

/// Release memory handed out by [`sbx_alloc`].
///
/// # Safety
///
/// `ptr` must have come from [`sbx_alloc`] with exactly this `len`, and must
/// not have been freed already. Passing a different `len` deallocates with the
/// wrong layout.
#[no_mangle]
pub unsafe extern "C" fn sbx_free(ptr: u32, len: u32) {
    if ptr == 0 || len == 0 {
        return;
    }
    if let Ok(layout) = Layout::from_size_align(len as usize, 1) {
        // SAFETY: this function's contract, plus the matching `layout`.
        dealloc(ptr as *mut u8, layout);
    }
}

/// Load a strategy document. Returns 0 on success, -1 on failure.
#[no_mangle]
pub extern "C" fn sbx_init(ptr: u32, len: u32) -> i32 {
    // SAFETY: the host allocated this range with `sbx_alloc`.
    let raw = unsafe { bytes(ptr, len) };
    let Ok(payload) = std::str::from_utf8(raw) else {
        return GUEST.with(|g| g.borrow_mut().fail("the init payload is not UTF-8"));
    };

    GUEST.with(|g| {
        let mut guest = g.borrow_mut();
        guest.error.clear();
        match guest.interpreter.init(payload) {
            Ok(()) => 0,
            Err(message) => guest.fail(message),
        }
    })
}

/// Evaluate one candle. Returns 0 on success, -1 on failure.
///
/// A signal, when there is one, leaves through `host_emit_signal` rather than
/// through a return buffer.
#[no_mangle]
pub extern "C" fn sbx_eval(ptr: u32, len: u32) -> i32 {
    // SAFETY: the host allocated this range with `sbx_alloc`.
    let raw = unsafe { bytes(ptr, len) };
    let header: Header = match serde_json::from_slice(raw) {
        Ok(header) => header,
        Err(e) => return GUEST.with(|g| g.borrow_mut().fail(format!("bad header: {e}"))),
    };

    let mut market = HostMarket;
    let outcome = GUEST.with(|g| {
        let mut guest = g.borrow_mut();
        guest.error.clear();
        guest.interpreter.eval(&header, &mut market)
    });

    match outcome {
        Ok(None) => 0,
        Ok(Some(signal)) => {
            let Ok(json) = serde_json::to_vec(&signal) else {
                return GUEST.with(|g| g.borrow_mut().fail("the signal could not be serialized"));
            };
            // SAFETY: `json` is a live local, so its bytes are in linear memory
            // for the duration of the call.
            let code = unsafe { host_emit_signal(json.as_ptr() as u32, json.len() as u32) };
            if code == 0 {
                0
            } else {
                GUEST.with(|g| {
                    g.borrow_mut()
                        .fail(format!("the host refused the signal (code {code})"))
                })
            }
        }
        Err(message) => GUEST.with(|g| g.borrow_mut().fail(message)),
    }
}

/// Pointer to the message behind the last `-1`, or 0 when there is none.
///
/// The host must read this before calling any other export: the next call may
/// reuse the buffer.
#[no_mangle]
pub extern "C" fn sbx_error_ptr() -> u32 {
    GUEST.with(|g| {
        let guest = g.borrow();
        if guest.error.is_empty() {
            0
        } else {
            guest.error.as_ptr() as u32
        }
    })
}

/// Length of the message behind the last `-1`, in bytes.
#[no_mangle]
pub extern "C" fn sbx_error_len() -> u32 {
    GUEST.with(|g| g.borrow().error.len() as u32)
}

/// The market source that reaches the host through `host_market_state`.
struct HostMarket;

impl MarketSource for HostMarket {
    fn view(&mut self, name: &str) -> Result<Option<TimeframeView>, String> {
        // SAFETY: `name` is a live `&str`, so its bytes are in linear memory.
        let ptr = unsafe { host_market_state(name.as_ptr() as u32, name.len() as u32) };
        if ptr < 0 {
            return Err(format!("the host declined the `{name}` view (code {ptr})"));
        }
        // `0` is "declared, but nothing has closed yet" -- warm-up, not a
        // refusal. `sbx_alloc` never returns 0 for a real allocation, so the
        // two cannot be confused.
        if ptr == 0 {
            return Ok(None);
        }
        let ptr = ptr as u32;

        // SAFETY: the host allocated `[len][payload]` with `sbx_alloc` and
        // wrote both parts before returning. `[u8; 4]` has alignment 1, so the
        // length is readable regardless of where the allocator placed it.
        let (payload, total) = unsafe {
            let len = u32::from_le_bytes(*((ptr as *const u8).cast::<[u8; 4]>()));
            (bytes(ptr + 4, len), 4 + len)
        };

        let parsed = serde_json::from_slice::<TimeframeView>(payload)
            .map_err(|e| format!("the host sent an unreadable `{name}` view: {e}"));

        // SAFETY: the same allocation, freed with the length the host used.
        unsafe { sbx_free(ptr, total) };

        parsed.map(Some)
    }
}
