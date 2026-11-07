//! `holosmith drain [--wait 10m]`: refuse new work and wait for the turns that are running (HS-051).
//!
//! The client half of [`crate::drain`], and deliberately small: one unix socket, one request line, then whatever
//! the server says until it says `result: <word>`. It is hand-written like [`crate::http::health::ask`] — a
//! socket, a read and a loop — because the alternative is a client library for a protocol this crate owns both
//! ends of.
//!
//! # Why the exit code is the `--wait`'s
//!
//! Spec item 10 step 3 loops the drain into a promotion that restarts the machine afterwards, so the one thing a
//! caller must be able to trust is "the machine is quiet, go" versus "it is not, do not": the terminal
//! `result: drained` is 0 and everything else — `expired` (a turn outlasted the wait), `refused`, a socket that
//! is not there, a server that went away mid-drain — is 1. The state is reported as well as the code, because the
//! operator's next move depends on which of those it was.
//!
//! # Why the lines are printed as they arrive
//!
//! A drain waits for a turn, which is minutes on a real machine. Printing what the server says as it says it is
//! the difference between a command that is working and one that looks hung, and it is the reason the server
//! writes progress lines at all.
//!
//! # What this command does not read
//!
//! No config file: the path is the admin directory ([`crate::deploy::ADMIN_DIR`], or `--admin-dir`), which is a
//! fixed path of the machine rather than a setting, and the wait is the flag's. A `--config` given to it is
//! therefore accepted and unused — the honest thing to do with a flag that names a file this command has no
//! question for, rather than refusing a command line that also serves `serve`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::deploy::ADMIN_DIR;
use crate::drain::{self, Ending};

/// How long the client waits for the server's answer before it calls the socket dead.
///
/// Not a deadline for the drain — that is `--wait`, and it is the server's — but the answer to "is anything
/// there at all": a socket whose server accepted and then wedged would otherwise hang this command for ever.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// What the command line asked for.
#[derive(Debug)]
pub struct DrainArgs {
    pub wait: Duration,
    /// The admin directory, or `None` for the machine's own.
    pub admin_dir: Option<PathBuf>,
}

pub fn run(args: DrainArgs) -> ExitCode {
    let admin_dir = args.admin_dir.unwrap_or_else(|| PathBuf::from(ADMIN_DIR));
    let path = drain::socket_path(&admin_dir);
    match ask(&path, args.wait) {
        Ok(ending) => {
            if ending.succeeded() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(sentence) => {
            eprintln!("holosmith: drain: {sentence}");
            ExitCode::FAILURE
        }
    }
}

/// Connect, ask, print every line, and answer how the drain ended.
///
/// The last line the server sends is `result: <word>`; everything before it is the account of what happened and
/// is printed as it arrives. A connection that ends without that line is a server that stopped mid-drain — a
/// `SIGTERM` or a crash — and says so rather than reporting the state it left.
fn ask(path: &Path, wait: Duration) -> Result<Ending, String> {
    let mut stream = UnixStream::connect(path).map_err(|error| {
        format!(
            "{} could not be connected to ({error}); the drain socket is only there while this machine's server \
             is running, and it is never reachable over the network",
            path.display()
        )
    })?;
    let _ = stream.set_read_timeout(Some(ANSWER_TIMEOUT));
    let _ = stream.set_write_timeout(Some(ANSWER_TIMEOUT));
    // Milliseconds on the wire: the server's own sequence measures the wait in a `Duration`, and a unit suffix
    // would be a second grammar for the same number (see [`crate::drain::Request::parse`]).
    let millis = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
    stream
        .write_all(format!("drain {millis}\n").as_bytes())
        .map_err(|error| {
            format!(
                "the request could not be sent to {} ({error})",
                path.display()
            )
        })?;
    stream.flush().map_err(|error| {
        format!(
            "the request could not be sent to {} ({error})",
            path.display()
        )
    })?;
    let mut ending = None;
    for line in BufReader::new(stream).lines() {
        let line = line.map_err(|error| {
            format!(
                "{} stopped answering mid-drain ({error}); the drain's own state is whatever the machine says now",
                path.display()
            )
        })?;
        println!("drain: {line}");
        if let Some(word) = line.strip_prefix("result: ") {
            ending = Some(Ending::from_word(word.trim()));
        }
    }
    match ending {
        Some(Some(ending)) => Ok(ending),
        // A word this build does not know is not a success: the client and the server are one binary, so this is
        // a version skew or a corrupted line, and neither is something to call drained.
        Some(None) => Err(format!(
            "{} answered a result this build does not know",
            path.display()
        )),
        None => Err(format!(
            "{} closed the connection before saying how the drain ended; the machine is draining if it said so, \
             and `holosmith drain --wait` will say again",
            path.display()
        )),
    }
}
