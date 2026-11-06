//! `holosmith server deploy`: install this machine as a server (HS-047, spec item 10).
//!
//! Thin on purpose, and thin in the same places `doctor` is: the config is read the way every command reads it (so
//! `--config` means one thing and a file with a problem is a refusal before any step runs), a role that cannot be
//! deployed refuses with one sentence, and everything an operator has to act on is in [`crate::deploy`]'s lines.
//!
//! Two things belong to the command rather than to the steps:
//!
//! - **The tail**, which the acceptance asks for: the URL this machine will serve, `holosmith key set <provider>`,
//!   and how a hub registers it. It prints after any run that reached the end — including one whose later steps
//!   were skipped — because it is the command's answer, not a step's (ADR-0056).
//! - **The resume command**, which a failed run prints as its last line: the command that just ran, with the same
//!   `--config` and `--root` made absolute and never `--dry-run`. Every step reads state first, so re-running is
//!   the whole recovery procedure, and a second syntax would be a second entry point (ADR-0055/ADR-0056).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::config::{Config, LoadError};
use crate::deploy::{self, Target};
use crate::docker::Docker;

/// What the command line asked for: the config every command reads, whether to apply anything, and the install
/// tree.
#[derive(Debug)]
pub struct DeployArgs {
    pub config: Option<PathBuf>,
    pub dry_run: bool,
    pub root: Option<PathBuf>,
}

pub fn run(args: DeployArgs) -> ExitCode {
    let loaded = match Config::load(args.config.clone()) {
        Ok(loaded) => loaded,
        Err(LoadError::Invalid(problems)) => {
            eprint!("{problems}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: server deploy: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(sentence) = deploy::refusal(&loaded.config) {
        eprintln!("holosmith: server deploy: {sentence}");
        return ExitCode::FAILURE;
    }
    let target = Target::resolve(args.root.clone());
    // The one config the steps see: the two roots moved under `--root`, and every machine fact untouched — which
    // is why this is computed here and handed over rather than recomputed inside each step (see `deploy::run`).
    let deployed = target.config(&loaded.config);
    let tools = deploy::Tools::default();
    let machine = Docker::new(deployed.docker_host.clone());
    let report = deploy::run(&deployed, &target, &tools, &machine, args.dry_run);
    print!("{}", report.render());
    if report.failed() {
        println!("{}", resume(&args));
        return ExitCode::FAILURE;
    }
    println!("deploy: url: {}", deployed.control_origin.as_str());
    println!("deploy: next: holosmith key set <provider>");
    println!(
        "deploy: next: register this machine in a hub (phase 3: `holosmith hub host add <name> <url>`)"
    );
    ExitCode::SUCCESS
}

/// The resume command: this run's own flags, absolute, and `--dry-run` **only when this run was one**.
///
/// **Only the flags the operator gave** — a bare resume means the default config path, and printing a `--config`
/// nobody typed would suggest the deploy chose one. The paths are made absolute for a reason the operator can act
/// on: a resume run from another directory must mean the same tree, and a relative `--root` would not. And a dry
/// run's resume is a dry run: the command that just ran changed nothing, so a resume that installed step 2 for
/// real would be a different command than the one that failed.
fn resume(args: &DeployArgs) -> String {
    let mut command = String::from("deploy: resume: holosmith server deploy");
    if let Some(config) = &args.config {
        command.push_str(&format!(" --config {}", absolute(config).display()));
    }
    if let Some(root) = &args.root {
        command.push_str(&format!(" --root {}", absolute(root).display()));
    }
    if args.dry_run {
        command.push_str(" --dry-run");
    }
    command
}

/// One path made absolute, or kept verbatim when the kernel cannot answer — a resume naming a path this process
/// could not resolve is better than no resume at all.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}
