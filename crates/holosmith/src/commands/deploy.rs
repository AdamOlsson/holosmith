//! `holosmith server deploy` and `holosmith hub deploy`: install one half of this machine (HS-047, HS-072).
//!
//! Thin on purpose, and thin in the same places `doctor` is: the config is read the way every command reads it (so
//! `--config` means one thing and a file with a problem is a refusal before any step runs), and everything an
//! operator has to act on is in [`crate::deploy`]'s lines.
//!
//! One function serves both commands, and [`crate::deploy::Kind`] is the whole difference: which half this run
//! installs, which steps it has no work for, and which group the service user joins. **Nothing is refused over a
//! role any more** — `server deploy` on a hub and `hub deploy` on a server are both ordinary runs, and the union
//! each one writes into `config.toml` is what makes the second one land (`deploy::union`).
//!
//! Three things belong to the command rather than to the steps:
//!
//! - **The role line**, which says what the machine is now: `deploy: role: both`, with the role the config said in
//!   brackets when this run changed it. Acceptance bullet 4 is "running both yields `both`", and a run that reached
//!   the union has to say so or the operator has to read the file to find out.
//! - **The tail**, the URL this machine will serve and the one or two next steps the half this run added earns.
//!   `key set` is a server's (a hub runs no key proxy) and `hub host add` is a hub's (a server registers *into* a
//!   hub and cannot reach its registry) — so a hub-only machine is not told to run a command whose only answer is
//!   "there is no key proxy here". It prints after any run that reached the end, because it is the command's answer
//!   rather than a step's (ADR-0056); a failed run prints the resume command instead.
//! - **The resume command**, which a failed run prints as its last line: the command that just ran, with the same
//!   `--config` and `--root` made absolute and never `--dry-run`. Every step reads state first, so re-running is the
//!   whole recovery procedure, and a second syntax would be a second entry point (ADR-0055/ADR-0056).

use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::{Config, LoadError, Role};
use crate::deploy::{self, Kind, Target};
use crate::docker::Docker;

/// What the command line asked for: the config every command reads, which half to install, whether to apply
/// anything, and the install tree.
#[derive(Debug)]
pub struct DeployArgs {
    pub config: Option<PathBuf>,
    pub kind: Kind,
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
            eprintln!("holosmith: {} deploy: {error}", args.kind);
            return ExitCode::FAILURE;
        }
    };
    let target = Target::resolve(args.root.clone());
    // The one config the steps see: the two roots moved under `--root`, and every machine fact untouched — which
    // is why this is computed here and handed over rather than recomputed inside each step (see `deploy::run_as`).
    // The role stays the machine's own; `run_as` puts the union in the copy it installs.
    let deployed = target.config(&loaded.config);
    let tools = deploy::Tools::default();
    let machine = Docker::new(deployed.docker_host.clone());
    let report = deploy::run_as(
        &deployed,
        &target,
        &tools,
        &machine,
        args.dry_run,
        args.kind,
    );
    print!("{}", report.render());
    if report.failed() {
        println!("{}", resume(&args));
        return ExitCode::FAILURE;
    }
    let role = deploy::union(loaded.config.role, args.kind);
    println!("{}", role_line(loaded.config.role, role, args.kind));
    println!("deploy: url: {}", deployed.control_origin.as_str());
    for next in next_steps(role) {
        println!("deploy: next: {next}");
    }
    ExitCode::SUCCESS
}

/// What the machine is now, and what this run changed about it — the acceptance's "yielding role `both`", as a line
/// rather than as a file the operator has to open.
fn role_line(found: Role, role: Role, kind: Kind) -> String {
    if found == role {
        return format!("deploy: role: {role}");
    }
    format!(
        "deploy: role: {role} (this machine's config said `{found}`, and `{} deploy` added the {} half)",
        kind.word(),
        kind.word()
    )
}

/// The next steps the role this run installed earns, in one order.
///
/// `key set` is a **server's**: the provider keys are what the key proxy swaps into a session's container, and a
/// hub has no key proxy at all. `hub host add` is a **hub's**: it registers a machine this one will forward to, and a
/// server cannot reach a hub's registry — a `server`-only machine's registry is a table nothing reads. A `both`
/// machine earns both, in that order.
fn next_steps(role: Role) -> Vec<String> {
    let mut steps = Vec::new();
    if role != Role::Hub {
        steps.push("holosmith key set <provider>".to_string());
    }
    if role != Role::Server {
        steps.push("holosmith hub host add <name> <url>".to_string());
    } else {
        // A server is registered *into* a hub, so its one next step names the other machine's command.
        steps.push("on the hub: holosmith hub host add <name> <url>".to_string());
    }
    steps
}

/// The resume command: this run's own flags, absolute, and `--dry-run` **only when this run was one**.
///
/// **Only the flags the operator gave** — a bare resume means the default config path, and printing a `--config`
/// nobody typed would suggest the deploy chose one. The paths are made absolute for a reason the operator can act
/// on: a resume run from another directory must mean the same tree, and a relative `--root` would not. And a dry
/// run's resume is a dry run: the command that just ran changed nothing, so a resume that installed step 2 for
/// real would be a different command than the one that failed.
fn resume(args: &DeployArgs) -> String {
    let mut command = format!("deploy: resume: holosmith {} deploy", args.kind);
    if let Some(config) = &args.config {
        command.push_str(&format!(" --config {}", deploy::absolute(config).display()));
    }
    if let Some(root) = &args.root {
        command.push_str(&format!(" --root {}", deploy::absolute(root).display()));
    }
    if args.dry_run {
        command.push_str(" --dry-run");
    }
    command
}
