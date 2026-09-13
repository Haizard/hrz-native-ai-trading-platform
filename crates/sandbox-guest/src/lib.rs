//! # `sandbox-guest`
//!
//! The Strategy Runtime **interpreter**, compiled to `wasm32-unknown-unknown`,
//! so the sandbox host can execute an untrusted strategy document without ever
//! compiling untrusted code (`docs/08-SANDBOX-WASM.md`).
//!
//! ## Why the interpreter, and not the strategy
//!
//! The obvious design is "compile the AI's strategy to WASM". This is not that,
//! and deliberately. A document is data: it names fields, functions and
//! comparisons from a closed vocabulary the validator already type-checked. So
//! the module that goes into the sandbox is *ours* -- built once, from source
//! in this repository -- and the untrusted part travels as input.
//!
//! That turns "can a generated strategy escape the sandbox" into "can a
//! document make the interpreter misbehave", which is a far smaller question,
//! and one a finite test suite can actually answer.
//!
//! ## Layout
//!
//! * [`interpret`] -- the whole decision. Portable Rust, unit-tested on the
//!   host, no FFI.
//! * `abi` -- `extern "C"` exports and the two allowlisted imports. Compiled
//!   for `wasm32` only.
//!
//! ## Building
//!
//! ```text
//! cargo build -p sandbox-guest --target wasm32-unknown-unknown --release
//! ```
//!
//! The `sandbox` crate does this for you from its `build.rs` and embeds the
//! result, so there is no artifact to keep in sync by hand.

#![deny(missing_docs)]

pub mod interpret;

#[cfg(target_arch = "wasm32")]
mod abi;
