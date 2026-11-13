//! `holosmith rollback`: the command surface of [`crate::rollback`].
//!
//! As thin as `upgrade`'s: it loads the config every command loads, resolves the install tree, refuses a rollback that
//! needs `root` and does not have it (`deploy::privilege_problem`, the rule the two release verbs share), and hands the
//! module the three facts it cannot work out itself — the `--to` tag, the drain's wait and the health window.
//!
//! One difference from `upgrade` is deliberate: the machine this command asks the machine through is
//! [`crate::promote::Real::joining`], not `Real::new`. A promotion refuses a drain somebody else started; a rollback
//! joins it, because a machine left draining by a promotion that failed after its drain is exactly the machine this
//! verb exists for (ADR-0083 D4). Its printed lines are prefixed `rollback:`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::config::{Config, LoadError};
use crate::deploy::Root;
use crate::promote::Real;
use crate::rollback::{self, Options};

/// What the command line asked for.
#[derive(Debug)]
pub struct RollbackArgs {
    /// The config file (`--config`), as every command takes it.
    pub config: Option<PathBuf>,
    /// The release to switch to, by tag; absent means the one behind the live release.
    pub to: Option<String>,
    /// How long the drain may wait for running turns.
    pub wait: Duration,
    /// The install tree, when this is not the machine's own.
    pub root: Option<PathBuf>,
}

pub fn run(args: RollbackArgs) -> ExitCode {
    let config = match Config::load(args.config.clone()) {
        Ok(loaded) => loaded.config,
        Err(LoadError::Invalid(problems)) => {
            eprintln!("holosmith: rollback: {}", format!("{problems}").trim_end());
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: rollback: the config could not be read: {error}");
            return ExitCode::FAILURE;
        }
    };
    let root = Root::resolve(args.root.clone());
    let options = Options {
        to: args.to,
        wait: args.wait,
        window: crate::promote::WINDOW,
    };
    let steps = Real::joining(&config);
    rollback::run(&config, &root, &options, &steps)
}
