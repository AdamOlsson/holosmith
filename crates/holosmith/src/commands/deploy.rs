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
use crate::remote::{self, Binary, Plan, Remote};

/// What the command line asked for: the config every command reads, which half to install, whether to apply
/// anything, and the install tree.
///
/// `ssh`/`binary` are the two flags that move the run to another machine ([`run_over_ssh`]). Over ssh, `config` and
/// `root` name **the target's** paths: they travel verbatim into the command the target runs, and this command does
/// not read the config at all.
#[derive(Debug)]
pub struct DeployArgs {
    pub config: Option<PathBuf>,
    pub kind: Kind,
    pub dry_run: bool,
    pub root: Option<PathBuf>,
    pub ssh: Option<String>,
    pub binary: Option<PathBuf>,
}

pub fn run(args: DeployArgs) -> ExitCode {
    if let Some(host) = args.ssh.clone() {
        return run_over_ssh(&args, &host);
    }
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
    println!(
        "{}",
        role_line(loaded.config.role, role, args.kind, args.dry_run)
    );
    println!("deploy: url: {}", deployed.control_origin.as_str());
    for next in next_steps(role) {
        println!("deploy: next: {next}");
    }
    ExitCode::SUCCESS
}

/// What the machine is now, and what this run changed about it — the acceptance's "yielding role `both`", as a line
/// rather than as a file the operator has to open.
///
/// **A dry run says `would be`, and that is not a nicety.** The line above the report says `deploy: dry run` and
/// nothing was written, so a past-tense `added the hub half` over a config that still says `server` would be the
/// report's first lie — the one thing the five words exist to prevent (ADR-0056). The role it *would* reach is still
/// worth printing: it is the answer to "what does this command do to me", and it is read from the config this run
/// really holds.
fn role_line(found: Role, role: Role, kind: Kind, dry_run: bool) -> String {
    if found == role {
        return format!("deploy: role: {role}");
    }
    match dry_run {
        true => format!(
            "deploy: role: {role} (would be: this machine's config says `{found}`, and `{} deploy` adds the {} half)",
            kind.word(),
            kind.word()
        ),
        false => format!(
            "deploy: role: {role} (this machine's config said `{found}`, and `{} deploy` added the {} half)",
            kind.word(),
            kind.word()
        ),
    }
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

/// The run that happens on another machine: send this binary there, verify the copy, and let the target's own
/// binary run the same command (HS-073, ADR-0078).
///
/// **The target's binary is the one that runs `deploy::run_as`**, with this run's `Kind`, `--root` and `--dry-run` —
/// so a `hub deploy --ssh` installs the hub's five steps and builds nothing, and there is no second step plan
/// anywhere in this file. This half owns the four `ssh` children ([`crate::remote`]), the three lines that say what
/// was sent where, and the exit code.
///
/// **The config is not loaded here, and the tail is not printed here.** Over ssh a config path names a file on the
/// target (a laptop with none can deploy), and `deploy: role:`/`url:`/`next:` are the target's own lines, printed by
/// the target's binary because the target measured them — a `deploy: url:` from this process would name a URL it
/// never asked about.
///
/// A child that exits non-zero is the target's answer rather than this command's failure to ask, so its code is
/// this command's exit code, and one line says so. A failure **before** the run child (an illegal host, a binary
/// that cannot be read, an architecture mismatch, a copy that does not verify) is refused with a sentence, and no
/// deploy has run on the target: the file to re-run is [`ssh_resume`]'s line.
fn run_over_ssh(args: &DeployArgs, host: &str) -> ExitCode {
    let remote = match Remote::parse(host) {
        Ok(remote) => remote,
        Err(sentence) => return refused(args, host, &sentence),
    };
    let binary = match Binary::read(args.binary.as_deref()) {
        Ok(binary) => binary,
        Err(sentence) => return refused(args, host, &sentence),
    };
    let tools = remote::Tools::default();
    let sent = match remote.send(&tools, &binary) {
        Ok(sent) => sent,
        Err(sentence) => return refused(args, host, &sentence),
    };
    for line in [sent.sent_line(), sent.verified_line()] {
        println!("{line}");
    }
    let plan = Plan {
        kind: args.kind,
        dry_run: args.dry_run,
        config: args.config.clone(),
        root: args.root.clone(),
    };
    println!("{}", sent.command_line(&plan));
    match remote.run(&tools, &sent, &plan) {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            println!(
                "{}`holosmith {} deploy` on {host} {}",
                remote::LINE_PREFIX,
                args.kind,
                ended(status)
            );
            println!("{}", ssh_resume(args, host));
            status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .map_or(ExitCode::FAILURE, ExitCode::from)
        }
        Err(sentence) => refused(args, host, &sentence),
    }
}

/// How the target's run ended, as one phrase.
///
/// A child that exits says `exited <n>`; one that a signal killed says so and names it, because `exited -1` is an
/// exit code that does not exist and this report's whole discipline is that a word means what it measured. A child
/// with neither (which a `wait` cannot produce today) is described rather than guessed at.
fn ended(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt as _;
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exited {code}"),
        (None, Some(signal)) => format!("was killed by signal {signal}"),
        (None, None) => "ended without an exit code".to_string(),
    }
}

/// One refusal before anything ran on the target: the sentence on stderr, and the command to re-run as the last
/// line on stdout — the local deploy's shape for a failed run, one hop out.
fn refused(args: &DeployArgs, host: &str, sentence: &str) -> ExitCode {
    eprintln!("{}{sentence}", remote::LINE_PREFIX);
    println!("{}", ssh_resume(args, host));
    ExitCode::FAILURE
}

/// The resume command for a run that went over ssh: the flags that named the transport, and the two paths
/// **verbatim**.
///
/// It is a different sentence from [`resume`]'s, and the difference is the point rather than an oversight: over ssh
/// `--config` and `--root` name **the target's** paths, so making them absolute here would name this machine's, and
/// a resume must name the same tree the failed run named. Both values are shell-quoted ([`remote::quote`]), because
/// this line is pasted into a shell and then travels through a second one: a path with a space in it must survive
/// both. `--binary` travels only when the operator gave one, for [`resume`]'s reason: a flag nobody typed would
/// suggest this command chose it.
fn ssh_resume(args: &DeployArgs, host: &str) -> String {
    let mut command = format!(
        "deploy: resume: holosmith {} deploy --ssh {host}",
        args.kind
    );
    if let Some(binary) = &args.binary {
        command.push_str(&format!(" --binary {}", binary.display()));
    }
    if let Some(config) = &args.config {
        command.push_str(&format!(
            " --config {}",
            remote::quote(&config.display().to_string())
        ));
    }
    if let Some(root) = &args.root {
        command.push_str(&format!(
            " --root {}",
            remote::quote(&root.display().to_string())
        ));
    }
    if args.dry_run {
        command.push_str(" --dry-run");
    }
    command
}
