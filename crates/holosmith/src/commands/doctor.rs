//! `holosmith doctor`: the machine's fitness, printed (spec item 10 §1 and §7).
//!
//! The command is thin on purpose: it reads the config the other commands read (so `--config` means the same
//! thing everywhere and a file with a problem is a refusal before any check runs), asks [`crate::doctor`] for the
//! report, prints it and answers with the exit code. Everything an operator has to act on is in the lines
//! themselves — that is the acceptance's "one actionable line per failure" — so nothing here reformats them.
//!
//! `--root DIR` is `server deploy --root`'s tree: the four install paths `doctor` reads — the config the
//! deploy wrote, the admin socket, the release tree and the unit — are built under `DIR` exactly as the deploy's
//! step 1 builds them, so a tree an unprivileged run installed can be examined without root and a check cannot
//! compare it against the machine's own `/etc` and `/opt` (one check later).

use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::{Config, LoadError};
use crate::deploy::Target;
use crate::docker::Docker;
use crate::doctor::{self, Depth};

/// What the command line asked for: the config file every command reads, how deep to look, and which install tree.
#[derive(Debug)]
pub struct DoctorArgs {
    pub config: Option<PathBuf>,
    pub depth: Depth,
    /// The install tree to examine; `None` is the machine's own paths.
    pub root: Option<PathBuf>,
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
    let target = Target::resolve(args.root);
    let config = target.config(&loaded.config);
    // The four machine-wide paths are the *target's*, which is the one thing `--root` changes: on the system paths
    // the rest of `Tools::default` is what a machine has, and under a tree these are the files the deploy wrote.
    let tools = doctor::Tools {
        config_path: target.etc_config(),
        admin_dir: target.admin_dir(),
        release_root: target.opt_dir(),
        unit_path: target.unit_path(),
        ..doctor::Tools::default()
    };
    // The daemon is only asked about by `--full`, and constructing the handle is what names the socket: a machine
    // without one still prints its whole quick list.
    let machine = Docker::new(config.docker_host.clone());
    let report = doctor::run(&config, args.depth, &tools, &machine);
    print!("{}", report.render());
    if report.failed() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
