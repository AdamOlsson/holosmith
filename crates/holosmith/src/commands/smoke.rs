//! `holosmith smoke`: the promotion's host smoke run (HS-077, spec item 10 step 2, ADR-0081).
//!
//! A **host** verb like `build`, and as thin: it reads the config the way every command does (so `--config` means
//! one thing and `docker_host` is honoured), resolves the binary to test and the root to work in, and hands
//! [`crate::smoke`] the three facts it cannot work out itself. What this module owns is the exit code and the
//! sentence an operator reads:
//!
//! * **0** the candidate passed — HS-078's `upgrade` reads this as "go".
//! * **1** a measurement failed, which is a candidate a promotion must not make live.
//! * **2** the run could not happen on this machine, which is a promotion that must not proceed *and* not a verdict
//!   about the binary. `doctor`'s `unknown` is the same distinction one check over (ADR-0054).
//!
//! The default for `--binary` is this executable — the story's own demo runs against the artifact this tree ships —
//! and HS-078 passes the candidate it is about to promote. Nothing here downloads, and nothing here runs on a host
//! without a daemon: that is an exit 2 with `doctor`'s own sentence.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::{Config, LoadError};
use crate::smoke;

/// What the command line asked for.
#[derive(Debug)]
pub struct SmokeArgs {
    pub config: Option<PathBuf>,
    /// The binary to test. Absent means this executable.
    pub binary: Option<PathBuf>,
    /// The scratch root. Absent means `<data_root>/smoke`.
    pub root: Option<PathBuf>,
}

pub fn run(args: SmokeArgs) -> ExitCode {
    let loaded = match Config::load(args.config.clone()) {
        Ok(loaded) => loaded,
        Err(LoadError::Invalid(problems)) => {
            eprintln!("holosmith: smoke: {}", format!("{problems}").trim_end());
            return ExitCode::from(1);
        }
        Err(error) => {
            eprintln!("holosmith: smoke: the config could not be read: {error}");
            return ExitCode::from(1);
        }
    };
    let binary = match args.binary {
        Some(path) => path,
        // This executable, which is what the story's own demo tests: the artifact the tree ships.
        None => match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                eprintln!(
                    "holosmith: smoke: this executable's own path could not be read ({error}), so `--binary` has \
                     to name the file to test"
                );
                return ExitCode::from(2);
            }
        },
    };
    let root = args
        .root
        .unwrap_or_else(|| loaded.config.data_root.join("smoke"));
    let outcome = smoke::run(&loaded.config, &binary, &root);
    if let Some(sentence) = outcome.sentence() {
        eprintln!("holosmith: smoke: {sentence}");
    }
    ExitCode::from(outcome.exit_code())
}
