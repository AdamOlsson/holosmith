//! `holosmith container snapshot`: spec item 9's script as a mode of this binary (HS-042).
//!
//! The container has `/work` as its working directory and `HS_SESSION_ID` in its environment (spec item 6's
//! run line), so this command needs no arguments: the worktree is `.`, the session id is the variable, and
//! the temporary index is a file under the container's `/tmp`. The host's delete flow calls
//! [`crate::snapshot::take`] directly with a data-root index instead.

use std::process::ExitCode;

use crate::snapshot;

pub fn run_snapshot() -> ExitCode {
    let id = match std::env::var("HS_SESSION_ID") {
        Ok(id) if !id.is_empty() => id,
        _ => {
            eprintln!("holosmith: container snapshot: HS_SESSION_ID is not set");
            return ExitCode::FAILURE;
        }
    };
    let worktree = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("holosmith: container snapshot: cannot read the working directory: {error}");
            return ExitCode::FAILURE;
        }
    };
    let index = std::env::temp_dir().join(format!("holosmith-wip-{id}.index"));
    match snapshot::take(&worktree, &index, &id) {
        Ok(taken) => {
            println!("{} {}", taken.as_str(), snapshot::reference(&id));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("holosmith: container snapshot: {error}");
            ExitCode::FAILURE
        }
    }
}
