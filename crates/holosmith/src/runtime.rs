//! What this run is, as opposed to what the machine is configured to be: the backend for
//! this invocation, and when the process started (ADR-0006's line between the two).
//!
//! A `Runtime` is never serialised, never read from the config file and never part of the
//! health payload: item 10's `--backend stub` picks the backend for one run, and the file
//! stays the one source of truth for the machine's settings.

use std::time::Instant;

use clap::ValueEnum;

/// Which backend executes sessions. `stub` is opt-in (ADR-0001), so the default is Docker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum Backend {
    /// The in-process stub backend: no Docker, no containers.
    Stub,
    #[default]
    Docker,
}

impl Backend {
    /// The word the startup line and a log line print.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stub => "stub",
            Self::Docker => "docker",
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// This run: the backend it chose, and the instant it started.
#[derive(Debug, Clone, Copy)]
pub struct Runtime {
    pub backend: Backend,
    started_at: Instant,
}

impl Runtime {
    /// A run that started now on `backend`.
    pub fn started(backend: Backend) -> Self {
        Self {
            backend,
            started_at: Instant::now(),
        }
    }

    /// Whole seconds since this run started.
    pub fn uptime_s(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backend_defaults_to_docker() {
        assert_eq!(Backend::default(), Backend::Docker);
        assert_eq!(Backend::default().as_str(), "docker");
    }

    #[test]
    fn the_two_backends_parse_and_print_the_same_word() {
        for (word, backend) in [("stub", Backend::Stub), ("docker", Backend::Docker)] {
            assert_eq!(
                Backend::from_str(word, true).expect("both backends are value-enum names"),
                backend
            );
            assert_eq!(backend.as_str(), word);
        }
    }

    #[test]
    fn a_runtime_starts_at_zero_and_carries_its_backend() {
        let runtime = Runtime::started(Backend::Stub);
        assert_eq!(runtime.backend.as_str(), "stub");
        assert_eq!(runtime.uptime_s(), 0);
    }
}
