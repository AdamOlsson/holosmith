//! `holosmith doctor`: the machine's fitness, printed (HS-045, spec item 10 §1 and §7).
//!
//! The command is thin on purpose: it reads the config the other commands read (so `--config` means the same
//! thing everywhere and a file with a problem is a refusal before any check runs), asks [`crate::doctor`] for the
//! report, prints it and answers with the exit code. Everything an operator has to act on is in the lines
//! themselves — that is the acceptance's "one actionable line per failure" — so nothing here reformats them.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::{Config, LoadError};
use crate::docker::Docker;
use crate::doctor::{self, Depth};

/// What the command line asked for: the config file every command reads, and how deep to look.
#[derive(Debug)]
pub struct DoctorArgs {
    pub config: Option<PathBuf>,
    pub depth: Depth,
}

pub fn run(args: DoctorArgs) -> ExitCode {
    let loaded = match Config::load(args.config) {
        Ok(loaded) => loaded,
        Err(LoadError::Invalid(problems)) => {
            eprint!("{problems}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: doctor: {error}");
            return ExitCode::FAILURE;
        }
    };
    let tools = doctor::Tools::default();
    // The daemon is only asked about by `--full`, and constructing the handle is what names the socket: a machine
    // without one still prints its whole quick list.
    let machine = Docker::new(loaded.config.docker_host.clone());
    let report = doctor::run(&loaded.config, args.depth, &tools, &machine);
    print!("{}", report.render());
    if report.failed() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
