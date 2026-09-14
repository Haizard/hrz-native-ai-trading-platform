//! # `chart-engine`
//!
//! The chart's scene builder, compiled to `wasm32-unknown-unknown` and driven
//! from plain JavaScript (`docs/14-FRONTEND-CHART-ENGINE.md`).
//!
//! ## The decision this crate implements
//!
//! `docs/14` asks for an explicit choice between a Rust-first frontend and a
//! React shell with a WASM chart module. The choice, recorded in that file on
//! 2026-09-14, is **Rust/WASM with a vanilla-JS shell**: the deployment is
//! Rust-only and worth keeping that way, the parts that matter are arithmetic
//! over `analytics-core`, and React would host the canvas without helping draw
//! it.
//!
//! ## No `wasm-bindgen`
//!
//! The module exports four functions -- [`alloc`], [`dealloc`],
//! [`build_scene`], and the accessors for the result buffer -- and the shell
//! copies a JSON request in and a JSON scene out. That is a few lines of glue on
//! each side, instead of a code generator, a CLI tool, and a version
//! requirement that has to match between them.
//!
//! The ABI is small enough to state completely:
//!
//! ```text
//! alloc(len: usize) -> *mut u8        caller writes the request JSON here
//! build_scene(ptr, len) -> i32        0 on success, non-zero on failure
//! scene_ptr() -> *const u8            the result, valid until the next call
//! scene_len() -> usize                its length in bytes
//! last_error_ptr() / last_error_len() why it failed, as UTF-8
//! dealloc(ptr, len)                   release an `alloc` buffer
//! ```
//!
//! ## The scene builder is testable without a browser
//!
//! [`scene`] is pure and has no wasm in it, so `cargo test -p chart-engine`
//! covers the geometry on the host. The shim below is `cfg(target_arch =
//! "wasm32")` and is not compiled for the host at all -- the same split
//! `sandbox-guest` uses, for the same reason.

#![deny(missing_docs)]

pub mod footprint;
pub mod scene;

pub use footprint::{layout as layout_footprint, Column as FootprintColumn, Grid};
pub use scene::{
    build, heikin_ashi, Bar, Cell, Level, Mode, Plot, Point, ProfileBar, Request, Scene, Tick,
};

/// The ABI the shell talks to.
///
/// `cfg`-gated to wasm because a `cdylib`'s exports are meaningless on the host
/// and `#[no_mangle]` would collide with the test binary's symbols.
#[cfg(target_arch = "wasm32")]
mod abi {
    use std::cell::RefCell;

    // The last scene, held so the shell can read it back. Kept alive between
    // calls rather than returned by pointer arithmetic: the shell reads
    // `scene_ptr`/`scene_len` immediately after `build_scene`, and the next
    // call replaces both.
    thread_local! {
        static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        static ERROR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    }

    /// Allocate `len` bytes for the caller to write into.
    ///
    /// # Safety
    /// The caller must pass the returned pointer back to [`dealloc`] with the
    /// same length, and must not read past `len`.
    #[no_mangle]
    pub extern "C" fn alloc(len: usize) -> *mut u8 {
        let mut buffer = Vec::<u8>::with_capacity(len);
        let pointer = buffer.as_mut_ptr();
        std::mem::forget(buffer);
        pointer
    }

    /// Release a buffer from [`alloc`].
    ///
    /// # Safety
    /// `ptr` must come from [`alloc`] with the same `len`, and must not be used
    /// afterwards.
    #[no_mangle]
    pub unsafe extern "C" fn dealloc(ptr: *mut u8, len: usize) {
        if !ptr.is_null() && len > 0 {
            // Reconstructing the Vec is what frees it; `with_capacity` used the
            // same allocator with the same capacity.
            drop(Vec::from_raw_parts(ptr, 0, len));
        }
    }

    /// Build a scene from a JSON request.
    ///
    /// Returns 0 on success and 1 on failure; on failure the reason is in
    /// [`last_error_ptr`]. A non-zero return is not an exception: the shell
    /// reads the message and shows it, rather than the module trapping and
    /// leaving the page with a dead canvas and no explanation.
    ///
    /// # Safety
    /// `ptr` must point at `len` readable bytes, as produced by [`alloc`].
    #[no_mangle]
    pub unsafe extern "C" fn build_scene(ptr: *const u8, len: usize) -> i32 {
        if ptr.is_null() || len == 0 {
            return fail("the request was empty");
        }
        let bytes = std::slice::from_raw_parts(ptr, len);
        let request: super::Request = match serde_json::from_slice(bytes) {
            Ok(request) => request,
            Err(e) => return fail(&format!("the request is not a valid scene request: {e}")),
        };

        match serde_json::to_vec(&super::scene::build(&request)) {
            Ok(scene) => {
                RESULT.with(|slot| *slot.borrow_mut() = scene);
                0
            }
            Err(e) => fail(&format!("the scene could not be serialized: {e}")),
        }
    }

    /// The scene from the last successful [`build_scene`].
    #[no_mangle]
    pub extern "C" fn scene_ptr() -> *const u8 {
        RESULT.with(|slot| slot.borrow().as_ptr())
    }

    /// Its length in bytes.
    #[no_mangle]
    pub extern "C" fn scene_len() -> usize {
        RESULT.with(|slot| slot.borrow().len())
    }

    /// The message from the last failure.
    #[no_mangle]
    pub extern "C" fn last_error_ptr() -> *const u8 {
        ERROR.with(|slot| slot.borrow().as_ptr())
    }

    /// Its length in bytes.
    #[no_mangle]
    pub extern "C" fn last_error_len() -> usize {
        ERROR.with(|slot| slot.borrow().len())
    }

    fn fail(message: &str) -> i32 {
        ERROR.with(|slot| *slot.borrow_mut() = message.as_bytes().to_vec());
        1
    }
}
