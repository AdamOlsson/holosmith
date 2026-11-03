//! What this run is, as opposed to what the machine is configured to be: the backend for
//! this invocation, and when the process started (ADR-0006's line between the two).
//!
//! A `Runtime` is never serialised, never read from the config file and never part of the
//! health payload: item 10's `--backend stub` picks the backend for one run, and the file
//! stays the one source of truth for the machine's settings.
//!
//! [`Backend`] is the one type both halves name: `serve` resolves `--backend` over the config
//! file's `backend` key and then builds a `Runtime` from it (HS-011, ADR-0015), so a run's choice
//! and the machine's setting are the same word rather than two.

use std::time::Instant;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Which backend executes sessions. `stub` is opt-in (ADR-0001), so the default is Docker.
///
/// Two spellings reach this enum and both are pinned by a test: the clap value name (what
/// `--backend` accepts) and the config file's string (what `backend` holds). Both are the
/// lowercase word [`Self::as_str`] prints, so an operator never has to guess a third.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[value(rename_all = "lower")]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// The in-process stub backend: no Docker, no containers.
    Stub,
    #[default]
    Docker,
}

impl Backend {
    /// Every backend, so the config's setter, the clap value list and the tests read one list
    /// rather than three copies of two enum variants.
    pub const ALL: [Backend; 2] = [Backend::Stub, Backend::Docker];

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
        assert_eq!(Backend::ALL.len(), 2, "ALL missed a backend");
        for (word, backend) in [("stub", Backend::Stub), ("docker", Backend::Docker)] {
            assert_eq!(
                Backend::from_str(word, true).expect("both backends are value-enum names"),
                backend
            );
            assert_eq!(backend.as_str(), word);
        }
    }

    #[test]
    fn the_config_file_spells_a_backend_the_same_word_the_flag_does() {
        // The config key and `--backend` must not be two vocabularies: a `backend = "Stub"` in the
        // file would otherwise be a value the flag refuses.
        for backend in Backend::ALL {
            let text = format!("\"{}\"", backend.as_str());
            assert_eq!(
                serde_json::to_string(&backend).expect("a backend is one JSON string"),
                text
            );
            let parsed: Backend =
                serde_json::from_str(&text).expect("the word it prints is the word it reads");
            assert_eq!(parsed, backend);
        }
    }

    #[test]
    fn a_runtime_starts_at_zero_and_carries_its_backend() {
        let runtime = Runtime::started(Backend::Stub);
        assert_eq!(runtime.backend.as_str(), "stub");
        assert_eq!(runtime.uptime_s(), 0);
    }
}
