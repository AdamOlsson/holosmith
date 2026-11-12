//! `holosmith container snapshot`: spec item 9's script as a mode of this binary.
//!
//! The container has `/work` as its working directory and `HS_SESSION_ID` in its environment (spec item 6's
//! run line), so this command needs no arguments: the worktree is `.`, the session id is the variable, and
//! the temporary index is a file under the container's `/tmp`. The host's delete flow calls
//! [`crate::snapshot::take`] directly with a data-root index instead.

use std::process::ExitCode;

use crate::fake_pi;
use crate::snapshot;

/// A session id as the database makes it. The id is joined into a temporary path, so the rule is
/// [`crate::sessions::is_id`]'s — the one home for "what is a session id".
fn valid_id(id: &str) -> bool {
    crate::sessions::is_id(id)
}

pub fn run_snapshot() -> ExitCode {
    let id = match std::env::var("HS_SESSION_ID") {
        Ok(id) if valid_id(&id) => id,
        Ok(id) => {
            eprintln!(
                "holosmith: container snapshot: HS_SESSION_ID {id:?} is not a 16-character session id"
            );
            return ExitCode::FAILURE;
        }
        Err(_) => {
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

/// `holosmith container fake-pi`: be pi for the smoke run (spec item 10 step 2).
///
/// Two answers and one job. `--version` is the pinned pi version, because `agent::version_of` asks the image's `pi`
/// which release it is and the smoke image's `pi` is this binary — answering the pin makes that check *measured*
/// through the real launch path instead of skipped. Every other argument is pi's and is ignored; the stdin/stdout
/// pair is the RPC channel and [`fake_pi::replay`] is the whole of it.
///
/// A refusal is on stderr with exit 1: a fixture this build may not replay (a newer schema) must fail the launch with
/// a sentence rather than print a turn whose frames are quietly incomplete.
pub fn run_fake_pi(pi_args: &[String]) -> ExitCode {
    if pi_args
        .iter()
        .any(|argument| argument == crate::agent::VERSION_ARGUMENT)
    {
        println!("{}", crate::pins::pi_version());
        return ExitCode::SUCCESS;
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match fake_pi::replay(stdin.lock(), stdout.lock()) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("holosmith: container fake-pi: {error}");
            ExitCode::FAILURE
        }
    }
}
