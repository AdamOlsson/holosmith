//! `holosmith hub`: deploy a hub, or manage the registry it forwards with (HS-072).
//!
//! Two very different commands behind one word because the spec's Commands table puts them there: `hub deploy` is
//! [`crate::commands::deploy`] with [`crate::deploy::Kind::Hub`] — the same seven-step report, five of the steps and
//! two honest skips — and it is dispatched there directly (`main`), because there is nothing for a wrapper to decide.
//! `hub host add|list|remove` is a **client** of this machine's own `/api/hosts` routes
//! ([`crate::hosts::client`]).
//!
//! Three things this module decides, and each is a sentence rather than a mechanism:
//!
//! - **A refusal prints the API's own `message`**, by `hostFailure`'s rule (ADR-0076 D6), so the CLI and the
//!   Computers screen cannot fork one refusal into two sentences.
//! - **A row prints [`crate::hosts::facts`]**, the same words `holosmith status` prints — one sentence for a machine's
//!   state, two commands.
//! - **Nothing is invented when nothing answers.** A machine whose `serve` is not running has no registry to add to,
//!   and the client's sentence names the address and the command that starts it.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::cli::HostAction;
use crate::clock;
use crate::config::{Config, LoadError};
use crate::hosts::{self, client};

/// `holosmith hub host …`: one call to this machine's own registry.
pub fn run_host(action: HostAction, config: Option<PathBuf>) -> ExitCode {
    let loaded = match Config::load(config) {
        Ok(loaded) => loaded,
        Err(LoadError::Invalid(problems)) => {
            eprint!("{problems}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: hub host: {error}");
            return ExitCode::FAILURE;
        }
    };
    let client = match client::Client::of(&loaded.config) {
        Ok(client) => client,
        Err(sentence) => {
            eprintln!("holosmith: hub host: {sentence}");
            return ExitCode::FAILURE;
        }
    };
    let now = clock::unix_now();
    match action {
        HostAction::Add { name, url } => match client.add(&name, &url) {
            Ok(row) => {
                print!("{}", row_line(&row, now));
                ExitCode::SUCCESS
            }
            Err(failure) => refuse(&failure),
        },
        HostAction::List => match client.list() {
            Ok(rows) => {
                println!("machines {}", rows.len());
                for row in &rows {
                    print!("{}", row_line(row, now));
                }
                ExitCode::SUCCESS
            }
            Err(failure) => refuse(&failure),
        },
        HostAction::Remove { name, force } => match client.remove(&name, force) {
            Ok(()) => {
                println!("removed {name}");
                ExitCode::SUCCESS
            }
            Err(failure) => refuse(&failure),
        },
    }
}

/// One row as `hub host` prints it: the same `machine <name> <facts>` line `status` prints, so one machine reads the
/// same in both commands.
fn row_line(row: &hosts::Host, now: i64) -> String {
    format!("machine {} {}\n", row.name, hosts::facts(row, now))
}

/// One refusal, printed the way ADR-0076 D6 says and nothing else: the code is [`client::Failure::sentence`]'s
/// business, and an unreachable machine is not a refusal at all.
fn refuse(failure: &client::Failure) -> ExitCode {
    eprintln!("holosmith: hub host: {}", failure.sentence());
    ExitCode::FAILURE
}
