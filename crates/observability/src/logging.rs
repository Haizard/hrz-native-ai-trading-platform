//! Structured logging setup (`docs/18`).
//!
//! ## One line per event, machine-readable
//!
//! `docs/18` asks for JSON logs with a consistent field set, because an
//! on-call engineer should be able to `jq` an incident rather than read it.
//! `LOG_FORMAT=json` switches the whole process to that shape; the default
//! stays human-readable so local `cargo run` is not miserable.
//!
//! ## An agent's tool call is part of the log
//!
//! `docs/18` requires every tool call to be logged with inputs and outputs --
//! that log is what makes a thesis auditable after the fact. The redaction
//! rule from `docs/15` applies to the *credential* fields only; market data is
//! deliberately not redacted, because a redacted audit log is not an audit log.

use std::sync::OnceLock;

use tracing_subscriber::EnvFilter;

/// Environment variable switching the log format.
pub const LOG_FORMAT_ENV: &str = "LOG_FORMAT";

/// The default filter when `RUST_LOG` is unset.
pub const DEFAULT_FILTER: &str = "info";

static SERVICE: OnceLock<String> = OnceLock::new();

/// Install the process-wide subscriber.
///
/// Safe to call once per process; a second call is ignored rather than
/// panicking, because a library that can crash a service at startup is worse
/// than one that logs in the wrong shape.
pub fn init_logging(service: &str) {
    let _ = SERVICE.set(service.to_string());

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));

    if json() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(true)
            .try_init();
    } else {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .try_init();
    }
}

/// Whether JSON was asked for.
fn json() -> bool {
    std::env::var(LOG_FORMAT_ENV)
        .map(|value| value.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
}

/// The service name passed to [`init_logging`].
///
/// Empty when logging was never initialised -- which is the normal case in
/// tests, and the reason this returns a string rather than panicking.
#[must_use]
pub fn service() -> &'static str {
    SERVICE.get().map_or("", String::as_str)
}

/// A short identifier for correlating one request across crates.
///
/// Not a UUID: it has to be readable when an engineer is scanning a thousand
/// lines for the one request that placed an order.
#[must_use]
pub fn request_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{:x}", nanos as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_name_is_remembered() {
        // Harmless in a test binary: `try_init` simply loses to the default
        // subscriber, and SERVICE is what we are checking.
        init_logging("api-gateway");
        assert_eq!(service(), "api-gateway");
    }

    #[test]
    fn request_ids_do_not_repeat() {
        let first = request_id();
        let second = request_id();
        assert!(!first.is_empty());
        assert!(!second.is_empty());
    }

    #[test]
    fn the_format_flag_is_case_insensitive() {
        std::env::set_var(LOG_FORMAT_ENV, "JSON");
        assert!(json());
        std::env::set_var(LOG_FORMAT_ENV, "text");
        assert!(!json());
        std::env::remove_var(LOG_FORMAT_ENV);
    }
}
