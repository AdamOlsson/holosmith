//! `holosmith upgrade <binary>`: the promotion pipeline as one command (spec item 10, ADR-0082).
//!
//! A **host** verb like `build` and `smoke`, and as thin: it loads the config the way every command does, resolves the
//! install tree, and hands [`crate::promote`] the four facts it cannot work out itself — the candidate, whether the
//! review was acknowledged, the drain's wait and the root. What this module owns is the command's surface: the flag
//! names and the exit code (0 only for a candidate that is live).
//!
//! # Whose account a promotion runs as
//!
//! Nothing here checks the account, and on a default install there is nothing to check: a promotion writes the
//! release tree and the `current` symlink under the root (`<home>/.local/share/holosmith`, unless `--root` or
//! `HOLOSMITH_ROOT` names another), writes the unit in the directory the operator's user manager reads, and restarts
//! through their own `systemctl --user` — so every one of the four is the running account's, and none asks a privilege
//! of anyone (ADR-0088 D2). A tree that account may not write is therefore a failure at the first write that cannot
//! be made rather than a refusal up front, and because the writes follow the drain, that failure is one of the ones
//! whose sentence has to say the machine is draining. Restoring or replacing the check this verb used to carry is the
//! install story's to decide (ADR-0088's consequences name it as outstanding); `--root DIR` names the tree, which is
//! how the gate and the demo drive the whole pipeline.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::config::{Config, LoadError};
use crate::deploy::Root;
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
    let root = Root::resolve(args.root.clone(), args.config.clone());
    let options = Options {
        candidate: args.binary,
        acknowledge_review: args.acknowledge_review,
        wait: args.wait,
        window: crate::promote::WINDOW,
    };
    let steps = Real::new(&config);
    promote::run(&config, &root, &options, &steps)
}
