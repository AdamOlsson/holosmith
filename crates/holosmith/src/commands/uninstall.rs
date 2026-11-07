//! `holosmith uninstall`: tear the install down and keep the data (HS-050).
//!
//! Thin, like the deploy it inverts: the config is read the way every command reads it, the target is what decides
//! where the tree is and whether each write is `sudo`-prefixed, and everything an operator has to act on is in
//! [`crate::uninstall`]'s lines.
//!
//! Two answers belong to the command rather than to the steps:
//!
//! - **A refusal is one sentence on stderr and exit 1, with no report and no resume command.** `--purge` over live
//!   sessions is an argument the command cannot honour, not a step that failed — running it again would be refused
//!   the same way, and the sentence already names the `--force` that overrides it.
//! - **A failed step prints the resume command**, `server deploy`'s shape: the command that just ran, with the same
//!   `--config` and `--root` made absolute, and `--purge`/`--force` only when this run had them. Every step reads
//!   before it applies, so re-running *is* the recovery.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::{Config, LoadError};
use crate::deploy::Target;
use crate::docker::Docker;
use crate::uninstall::{self, Args};

/// What the command line asked for: the config every command reads, the install tree, and the two flags.
#[derive(Debug)]
pub struct UninstallArgs {
    pub config: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub purge: bool,
    pub force: bool,
}

pub fn run(args: UninstallArgs) -> ExitCode {
    let loaded = match Config::load(args.config.clone()) {
        Ok(loaded) => loaded,
        Err(LoadError::Invalid(problems)) => {
            eprint!("{problems}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: uninstall: {error}");
            return ExitCode::FAILURE;
        }
    };
    let target = Target::resolve(args.root.clone());
    // The one config the steps see: the two roots moved under `--root`, and every machine fact untouched — the same
    // contract `server deploy` hands its steps (see `deploy::run`).
    let deployed = target.config(&loaded.config);
    let tools = uninstall::Tools::default();
    let machine = Docker::new(deployed.docker_host.clone());
    let wanted = Args {
        purge: args.purge,
        force: args.force,
    };
    let report = match uninstall::run(&deployed, &target, &tools, &machine, &wanted) {
        Ok(report) => report,
        Err(sentence) => {
            eprintln!("holosmith: uninstall: {sentence}");
            return ExitCode::FAILURE;
        }
    };
    print!("{}", report.render());
    if report.failed() {
        println!("{}", resume(&args));
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// The resume command: this run's own flags, absolute, and nothing it was not given.
///
/// The deploy's function with the teardown's two flags instead of `--dry-run` — `--config` and `--root` are how a
/// re-run means the same tree, and a flag the operator did not type would suggest the command chose it.
fn resume(args: &UninstallArgs) -> String {
    let mut command = String::from("uninstall: resume: holosmith uninstall");
    if let Some(config) = &args.config {
        command.push_str(&format!(
            " --config {}",
            crate::deploy::absolute(config).display()
        ));
    }
    if let Some(root) = &args.root {
        command.push_str(&format!(
            " --root {}",
            crate::deploy::absolute(root).display()
        ));
    }
    if args.purge {
        command.push_str(" --purge");
    }
    if args.force {
        command.push_str(" --force");
    }
    command
}
