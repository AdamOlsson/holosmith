//! Diagnostics: `tracing` to stderr, with the filter taken from `HOLOSMITH_LOG` (ADR-0007).
//!
//! The filter is not a config key: it belongs to the invocation — a systemd unit, a debug
//! session — rather than to the machine's settings that `server deploy` writes. No log file
//! is written either: `holosmith logs` reads journald, and a session's durable events are
//! data, never diagnostics.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// The filter used when `HOLOSMITH_LOG` is unset or unparsable.
pub const DEFAULT_FILTER: &str = "info";

/// Install stderr logging and return the filter that took effect, for the first startup line.
///
/// `try_init` rather than `init`, so a test that calls this twice is not a panic.
pub fn init() -> String {
    let (filter, effective) = match EnvFilter::try_from_env("HOLOSMITH_LOG") {
        Ok(filter) => {
            let text = filter.to_string();
            (filter, text)
        }
        Err(_) => (EnvFilter::new(DEFAULT_FILTER), DEFAULT_FILTER.to_string()),
    };
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .finish();
    let _ = tracing_subscriber::util::SubscriberInitExt::try_init(subscriber);
    effective
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_filter_is_info() {
        assert_eq!(DEFAULT_FILTER, "info");
    }

    #[test]
    fn a_second_init_is_not_a_panic_and_still_names_the_filter() {
        let first = init();
        let second = init();
        assert_eq!(first, second);
        assert!(!first.is_empty());
    }
}
