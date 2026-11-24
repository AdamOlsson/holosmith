//! `holosmith update`: a pass of the gate, then the promotion ([`crate::pass`], ADR-0110).
//!
//! Thin, like `upgrade`, and thin in the same way: it loads the config the way every command does, resolves the
//! install tree, and hands [`crate::pass`] the two facts it cannot work out itself — where the report goes and how
//! long the drain may wait. What this module owns is the command's surface: the flag names and the exit code (0 only
//! for a candidate that is live).
//!
//! `--acknowledge-review` is not here on purpose: the gate is the record (ADR-0110 D2). `upgrade` keeps its flag,
//! because a human handing this machine a file is a different act from a machine proving a tree.
//!
//! The machine a promotion asks is built from [`crate::pass::VERB`] rather than from a literal, so the pipeline's
//! drain lines say `update:` and no line of an `update` run can say `upgrade:`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::config::{Config, LoadError};
use crate::deploy::{self, Root};
use crate::pass::{self, Options};
use crate::promote::Real;

/// What the command line asked for.
#[derive(Debug)]
pub struct UpdateArgs {
    /// The config file (`--config`), as every command takes it.
    pub config: Option<PathBuf>,
    /// Where the run's report goes (`--report`), when a caller asked for one.
    pub report: Option<PathBuf>,
    /// How long the drain may wait for the turns that are running.
    pub wait: Duration,
    /// The install tree, when this is not the machine's own.
    pub root: Option<PathBuf>,
}

pub fn run(args: UpdateArgs) -> ExitCode {
    let config = match Config::load(args.config.clone()) {
        Ok(loaded) => loaded.config,
        Err(LoadError::Invalid(problems)) => {
            let said = format!("{problems}");
            return refused_ending(args.report.as_deref(), said.trim_end());
        }
        Err(error) => {
            return refused_ending(
                args.report.as_deref(),
                &format!("the config could not be read: {error}"),
            );
        }
    };
    let root = Root::resolve(args.root.clone(), args.config.clone());
    let options = Options {
        report: args.report,
        wait: args.wait,
    };
    let steps = Real::prefixed(&config, pass::VERB);
    pass::run(&config, &root, &options, &steps, &deploy::Tools::default())
}

/// An ending that happens before [`pass::run`] can begin one: the sentence is printed, and the report is written when
/// `--report` named a file.
///
/// This is the arm `--report` is easiest to forget in and hardest to live without: a job's caller polls that file, so a
/// config it cannot load would otherwise leave a caller with nothing to distinguish it from a job that crashed. A
/// report that cannot be written is said in one sentence and changes no exit code — the run had already ended 1.
fn refused_ending(report: Option<&Path>, said: &str) -> ExitCode {
    eprintln!("holosmith: {}: {said}", pass::VERB);
    if let Some(path) = report
        && let Err(error) = pass::Report::refused(said).write(path)
    {
        eprintln!("holosmith: {}: {error}", pass::VERB);
    }
    ExitCode::FAILURE
}
