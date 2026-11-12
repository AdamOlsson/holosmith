//! `holosmith upgrade <binary>`: the promotion pipeline as one command (HS-078, spec item 10, ADR-0082).
//!
//! A **host** verb like `build` and `smoke`, and as thin: it loads the config the way every command does, resolves the
//! install tree, refuses a promotion that needs `root` and does not have it, and hands [`crate::promote`] the four
//! facts it cannot work out itself — the candidate, whether the review was acknowledged, the drain's wait and the
//! root. What this module owns is the command's surface: the flag names, the one refusal that is about *privilege*
//! rather than about the candidate, and the exit code (0 only for a candidate that is live).
//!
//! # Why a privileged install must be root
//!
//! A promotion writes the release tree (`/opt/holosmith`), the `current` symlink, the unit file and then restarts a
//! system service. `server deploy` writes each of its own files as a `sudo`-prefixed child, and a promotion that did
//! the same would be a dozen escalations for one command, each asking for the same credential. So the rule is one
//! check with one sentence: run it as root, or name an install tree with `--root` (which is how the gate and the demo
//! drive the whole pipeline, since neither may write `/etc` or needs systemd).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::config::{Config, LoadError};
use crate::deploy::Target;
use crate::promote::{self, Options, Real};

/// What the command line asked for.
#[derive(Debug)]
pub struct UpgradeArgs {
    /// The config file (`--config`), as every command takes it.
    pub config: Option<PathBuf>,
    /// The candidate binary.
    pub binary: PathBuf,
    /// Item 12's review, acknowledged.
    pub acknowledge_review: bool,
    /// How long the drain may wait for running turns.
    pub wait: Duration,
    /// The install tree, when this is not the machine's own.
    pub root: Option<PathBuf>,
}

pub fn run(args: UpgradeArgs) -> ExitCode {
    let config = match Config::load(args.config.clone()) {
        Ok(loaded) => loaded.config,
        Err(LoadError::Invalid(problems)) => {
            eprintln!("holosmith: upgrade: {}", format!("{problems}").trim_end());
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: upgrade: the config could not be read: {error}");
            return ExitCode::FAILURE;
        }
    };
    let target = Target::resolve(args.root.clone());
    if target.privileged() && !is_root() {
        eprintln!(
            "holosmith: upgrade: a promotion writes the release tree ({}), the `{}` symlink and the unit, and \
             restarts {} — so it runs as root, or with `--root DIR` for an install tree this user owns: nothing was \
             promoted",
            target.opt_dir().display(),
            crate::release::CURRENT,
            crate::deploy::UNIT_NAME
        );
        return ExitCode::FAILURE;
    }
    let options = Options {
        candidate: args.binary,
        acknowledge_review: args.acknowledge_review,
        wait: args.wait,
        window: crate::promote::WINDOW,
    };
    let steps = Real::new(&config);
    promote::run(&config, &target, &options, &steps)
}

/// Whether this process's effective uid is 0, read from `/proc/self/status`.
///
/// One file rather than a child (`id -u`) and rather than a dependency: `Uid:`'s second field is the effective uid,
/// and this build is Linux-only (both release targets are). A file that cannot be read answers `false`, which is the
/// safe direction — a promotion that wrongly believed it could write `/opt` would fail at the first write with a
/// `Permission denied` instead of the sentence above.
fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Uid:"))
                .and_then(|fields| fields.split_whitespace().nth(1).map(str::to_string))
        })
        .is_some_and(|uid| uid == "0")
}
