//! The capability allowlist, checked against a module's import section.
//!
//! ## Why this is a check and not a policy
//!
//! The usual way to sandbox is to link a lot of host functions and then decide
//! at runtime which calls to permit. That puts the decision *inside* the thing
//! being defended against: a bug in the check is a hole, and the host function
//! is still reachable.
//!
//! Here the decision happens before anything is linked. A module that imports
//! `wasi_snapshot_preview1.fd_write` is refused while it is still bytes -- it is
//! never instantiated, so there is no instance whose `fd_write` could be
//! reached, and no runtime path that could catch the refusal and continue. That
//! is what `docs/08-SANDBOX-WASM.md` means by "compile/link failure, not a
//! runtime panic that could be caught and ignored".
//!
//! The list is a closed set of four names, and the module they must come from is
//! fixed, so an import cannot smuggle itself in under a different namespace.

use wasmtime::{ExternType, Module};

use crate::error::SandboxError;

/// The only module an import may come from.
pub const ALLOWED_IMPORT_MODULE: &str = "env";

/// Every host function a sandboxed module may import.
///
/// Four names, and nothing else. What each one is for:
///
/// * `host_market_state` -- read a declared timeframe's view.
/// * `host_emit_signal` -- emit a signal.
/// * `host_state_get` / `host_state_set` -- the strategy's own scoped
///   key-value store. Nothing in the DSL reads or writes it yet; it is on the
///   list because the spec's allowlist names it, and because adding a stateful
///   construct later should not be a change to the sandbox boundary.
///
/// Deliberately absent, and unrepresentable as a result: filesystem access,
/// network access, process or thread spawning, environment variables, access to
/// another strategy's state, and anything resembling a secret.
pub const ALLOWED_HOST_FUNCTIONS: &[&str] = &[
    "host_market_state",
    "host_emit_signal",
    "host_state_get",
    "host_state_set",
];

/// Whether an import is permitted.
#[must_use]
pub fn is_allowed(module: &str, field: &str) -> bool {
    module == ALLOWED_IMPORT_MODULE && ALLOWED_HOST_FUNCTIONS.contains(&field)
}

/// Refuse `module` unless every import it declares is on the allowlist.
///
/// # Errors
///
/// [`SandboxError::CapabilityDenied`] naming every offending import, so a
/// refusal says what was asked for rather than just that something was.
pub fn check_module(module: &Module) -> Result<(), SandboxError> {
    let mut denied: Vec<String> = Vec::new();

    for import in module.imports() {
        let from = import.module();
        let field = import.name();

        if !is_allowed(from, field) {
            denied.push(format!("{from}.{field}"));
            continue;
        }
        // An allowed *name* is not enough: a module could try to import
        // `host_emit_signal` as a global or a table and have the host link
        // something else into the slot. Only functions are on the list.
        if !matches!(import.ty(), ExternType::Func(_)) {
            denied.push(format!("{from}.{field} (imported as a non-function)"));
        }
    }

    if denied.is_empty() {
        Ok(())
    } else {
        Err(SandboxError::CapabilityDenied(denied.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::{Engine, Module};

    fn module(wat: &str) -> Module {
        Module::new(
            &Engine::new(&crate::instance::engine_config()).unwrap(),
            wat,
        )
        .expect("the fixture is valid wat")
    }

    #[test]
    fn the_shipped_interpreter_is_allowed() {
        // The real artifact, not a fixture. If the guest ever grows an import
        // this test is what notices.
        let engine = Engine::new(&crate::instance::engine_config()).unwrap();
        let module = Module::new(&engine, crate::instance::guest_wasm()).expect("valid module");
        check_module(&module).expect("the shipped guest must be on the allowlist");
    }

    #[test]
    fn a_module_that_imports_nothing_is_allowed() {
        check_module(&module(r#"(module (func (export "f")))"#)).unwrap();
    }

    #[test]
    fn each_allowlisted_function_may_be_imported() {
        for name in ALLOWED_HOST_FUNCTIONS {
            let wat =
                format!(r#"(module (import "env" "{name}" (func (param i32) (result i32))))"#);
            check_module(&module(&wat))
                .unwrap_or_else(|e| panic!("`{name}` should be allowed: {e}"));
        }
    }

    #[test]
    fn wasi_is_denied() {
        let err = check_module(&module(
            r#"(module (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32))))"#,
        ))
        .unwrap_err();
        assert!(matches!(err, SandboxError::CapabilityDenied(_)));
        assert!(err.to_string().contains("fd_write"), "{err}");
    }

    #[test]
    fn a_plausible_host_function_from_the_wrong_module_is_denied() {
        // Right name, wrong namespace: the module is part of the check.
        let err = check_module(&module(
            r#"(module (import "wasi" "host_emit_signal" (func (param i32))))"#,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("wasi.host_emit_signal"), "{err}");
    }

    #[test]
    fn an_allowlisted_name_imported_as_a_global_is_denied() {
        let err = check_module(&module(
            r#"(module (import "env" "host_emit_signal" (global i32)))"#,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("non-function"), "{err}");
    }

    #[test]
    fn every_denial_is_reported_not_just_the_first() {
        let err = check_module(&module(
            r#"(module
                 (import "env" "fs_read" (func))
                 (import "env" "net_connect" (func))
                 (import "env" "exec" (func)))"#,
        ))
        .unwrap_err();
        let message = err.to_string();
        for expected in ["fs_read", "net_connect", "exec"] {
            assert!(
                message.contains(expected),
                "{expected} missing from: {message}"
            );
        }
    }
}
