//! Compile `sandbox-guest` to WASM and embed the result.
//!
//! ## Why a build script and not a committed artifact
//!
//! The sandbox host is useless without a guest module, so the two must not be
//! able to drift. Building the guest here means there is no `.wasm` in the
//! repository to forget to regenerate: the module inside the binary is always
//! the one this commit's source produces.
//!
//! ## Why the nested build gets its own target directory
//!
//! Cargo takes an exclusive lock on the target directory for the duration of a
//! build. A build script that ran `cargo build` against the same directory would
//! block forever waiting for the lock its own parent is holding. A separate
//! `--target-dir` sidesteps it entirely, at the cost of rebuilding the guest's
//! (small) dependency graph the first time.
//!
//! ## Why the change list is spelled out
//!
//! By default a build script re-runs only when files *in its own package*
//! change, which would leave the embedded module stale whenever a crate the
//! guest depends on was edited. Every source directory on the guest's path is
//! therefore declared below.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Directories whose contents the guest is built from.
const GUEST_SOURCES: &[&str] = &[
    "crates/sandbox-guest/src",
    "crates/strategy-runtime/src",
    "crates/strategy-dsl/src",
    "crates/analytics-core/src",
];

/// Manifests whose contents decide how the guest is built.
const GUEST_MANIFESTS: &[&str] = &[
    "crates/sandbox-guest/Cargo.toml",
    "crates/strategy-runtime/Cargo.toml",
    "crates/strategy-dsl/Cargo.toml",
    "crates/analytics-core/Cargo.toml",
    "Cargo.toml",
];

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/sandbox has a grandparent")
        .to_path_buf();

    for relative in GUEST_SOURCES.iter().chain(GUEST_MANIFESTS) {
        println!("cargo:rerun-if-changed={}", root.join(relative).display());
    }

    let guest_target_dir = root.join("target").join("sandbox-guest");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());

    let status = Command::new(&cargo)
        .args([
            "build",
            "--package",
            "sandbox-guest",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
            "--target-dir",
        ])
        .arg(&guest_target_dir)
        .current_dir(&root)
        .status();

    let status = match status {
        Ok(status) => status,
        Err(error) => panic!(
            "could not run `{cargo}` to build the sandbox guest: {error}\n\
             The `sandbox` crate needs a working cargo on PATH."
        ),
    };
    if !status.success() {
        panic!(
            "building the sandbox guest failed ({status}).\n\
             If the error above mentions the target, install it with:\n\
             \x20   rustup target add wasm32-unknown-unknown"
        );
    }

    let built = guest_target_dir
        .join("wasm32-unknown-unknown")
        .join("release")
        .join("sandbox_guest.wasm");
    let bytes = std::fs::read(&built)
        .unwrap_or_else(|error| panic!("the guest module {} is missing: {error}", built.display()));

    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("set by cargo")).join("sandbox_guest.wasm");
    std::fs::write(&out, &bytes).expect("OUT_DIR is writable");

    // A canary for the check in CI that the sandbox links no YAML parser. If a
    // future dependency drags one in, this fails the build instead of silently
    // widening the sandbox's attack surface.
    if contains(&bytes, b"unsafe-libyaml") || contains(&bytes, b"serde_yaml") {
        panic!(
            "the sandbox guest contains a YAML parser. The guest must read JSON only \
             (docs/08-SANDBOX-WASM.md) -- check for a dependency re-enabling \
             `strategy-dsl/yaml` on the guest's path."
        );
    }

    println!("cargo:rustc-env=SANDBOX_GUEST_WASM={}", out.display());
    println!("cargo:rustc-env=SANDBOX_GUEST_BYTES={}", bytes.len());
}

/// Whether `haystack` contains `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
