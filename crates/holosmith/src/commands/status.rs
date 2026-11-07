//! `holosmith status`: the machine's four facts, printed (HS-050, spec's Commands table).
//!
//! Thin on purpose, and thin in the same place `doctor` is: the config is read the way every command reads it (so
//! `--config` means one thing and a file with a problem is a refusal before anything is measured), one daemon
//! handle is built for the proxy lines, and everything an operator has to act on is in [`crate::status`]'s lines.
//!
//! The exit code is [`crate::status::Report::failed`]'s: 1 iff a fact could not be **read**. An `unknown` line — no
//! server answering, no host registry — is a report about a machine, which is what this command is for.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::{Config, LoadError};
use crate::docker::Docker;
use crate::status;

/// What the command line asked for: the config file every command reads.
#[derive(Debug)]
pub struct StatusArgs {
    pub config: Option<PathBuf>,
}

pub fn run(args: StatusArgs) -> ExitCode {
    let loaded = match Config::load(args.config) {
        Ok(loaded) => loaded,
        Err(LoadError::Invalid(problems)) => {
            eprint!("{problems}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: status: {error}");
            return ExitCode::FAILURE;
        }
    };
    // The daemon is asked one question — is this container up — and constructing the handle is what names the
    // socket: a machine without a daemon still prints every line, with both proxies `not running`.
    let machine = Docker::new(loaded.config.docker_host.clone());
    let report = status::run(&loaded.config, &machine);
    print!("{}", report.render());
    if report.failed() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
