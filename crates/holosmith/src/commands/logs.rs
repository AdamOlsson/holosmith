//! `holosmith logs`: the server's log from journald, or one session's `session.log` (HS-050).
//!
//! Thin, like the other commands: the one seam is [`crate::logs::Tools`], and the exit code is the child's own for
//! the journald arm — journald's "no journal" answer is a fact this command has no better words for. The config is
//! read **only for `--session`**, because the journald arm needs nothing from it: a machine with a broken config is
//! one whose log an operator most wants to read.
//!
//! `--session` and `-f` combine: a followed session log prints what is there and then what arrives
//! ([`crate::logs::follow`]).

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::{Config, LoadError};
use crate::logs;

/// What the command line asked for.
#[derive(Debug)]
pub struct LogsArgs {
    pub config: Option<PathBuf>,
    /// `-f`: keep printing what arrives.
    pub follow: bool,
    /// `--session ID`: that session's log instead of the unit's.
    pub session: Option<String>,
}

pub fn run(args: LogsArgs) -> ExitCode {
    // The journald arm comes **first and reads no config**: the unit's log is journald's, so a machine whose
    // `config.toml` is broken can still be read — which is exactly the machine an operator runs `logs` on (HS-050's
    // review, S4). The session arm is where a data root is needed, and that is where the config is read.
    let Some(id) = args.session else {
        let tools = logs::Tools::default();
        return match logs::server(args.follow, &tools) {
            // The child's own code, because its refusal is its answer: a machine with no journal answers non-zero
            // and its line is the explanation. A child killed by a signal has no code, and that is a failure.
            Ok(code) => u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from),
            Err(sentence) => {
                eprintln!("holosmith: logs: {sentence}");
                ExitCode::FAILURE
            }
        };
    };
    let loaded = match Config::load(args.config) {
        Ok(loaded) => loaded,
        Err(LoadError::Invalid(problems)) => {
            eprint!("{problems}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: logs: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut out = std::io::stdout().lock();
    match logs::session(&id, args.follow, &loaded.config, &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(sentence) => {
            // The `?`-less arm: nothing was printed, so the sentence is the whole answer.
            let _ = out.flush();
            eprintln!("holosmith: logs: {sentence}");
            ExitCode::FAILURE
        }
    }
}
