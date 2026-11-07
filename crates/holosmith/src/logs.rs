//! `holosmith logs`: the server's log, or one session's (HS-050, spec's Commands table).
//!
//! Two sources, and they are read in two different ways on purpose:
//!
//! - **The server's log is journald's.** The spec says so ("Server log from journald"), and the unit systemd
//!   loads is named by [`crate::deploy::UNIT_NAME`], so the read is one child: `journalctl -u holosmith.service`,
//!   with `-f` appended when the operator asked to follow. Its stdout is inherited, not captured — following is
//!   the child's job and a captured pipe would deliver nothing until journald exited — and **no `-n`**: any
//!   window would be a truncation this command cannot label, and `journalctl`'s own pager is what an operator
//!   navigates. A child that cannot be spawned, and a child that exits non-zero, are its own answer: the command
//!   prints one sentence naming what was run and hands back the code.
//! - **A session's log is a file this build wrote.** `<data>/sessions/<id>/session.log` is written by
//!   [`crate::exit::Exits::record`] (the launch line, and the stderr's tail when a container failed), so reading it
//!   is that writer's inverse and needs no program: the reader is `std::fs`, and `-f` is
//!   [`follow`]'s poll loop. A `tail` child here would make the command's only unmeasurable dependency a program
//!   that does not exist on a minimal machine, for a file it owns.
//!
//! The two flags combine, and that is the one thing the story's acceptance says about `-f` beyond "streams": a
//! followed session log prints what is there and then what arrives.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::Config;
use crate::deploy::UNIT_NAME;
use crate::exit::SessionLog;
use crate::sessions::is_id;

/// How often a followed session log is re-read: five times a second is what a terminal reads as "live" and what
/// one core can afford for a file the process is already writing.
pub const TICK: Duration = Duration::from_millis(200);

/// Which log the operator asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// journald's log of the unit.
    Server,
    /// One session's `session.log`, by id.
    Session(String),
}

/// The program the server arm runs, as a seam.
///
/// A `PathBuf` for [`crate::preflight::Tools`]'s reason: this crate denies `unsafe` and so cannot set `PATH`,
/// which leaves naming the program as the only way a test or a gate can point this at a stand-in.
#[derive(Debug, Clone)]
pub struct Tools {
    pub journalctl: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            journalctl: PathBuf::from("journalctl"),
        }
    }
}

/// The child's arguments: the unit systemd loads, and `-f` when the operator wants to follow.
///
/// A function rather than a `match` inside the spawn, so the argv the gate asserts and the argv the command runs
/// are the same expression — and `-u` before its value, which is what `journalctl` accepts and what an operator
/// would type.
pub fn arguments(follow: bool) -> Vec<&'static str> {
    let mut arguments = vec!["-u", UNIT_NAME];
    if follow {
        arguments.push("-f");
    }
    arguments
}

/// The server's log: one `journalctl` child with this process's own stdio, and its exit code as the answer.
///
/// The child's code is returned rather than folded into a sentence: journald's own "no journal" answer is a fact
/// about the machine that the command has no better words for, and a `logs` that turned it into a generic failure
/// would hide the one line that explains it. Only a child that could not be started at all is an `Err`, because
/// then there is no answer to pass on.
pub fn server(follow: bool, tools: &Tools) -> Result<i32, String> {
    let status = std::process::Command::new(&tools.journalctl)
        .args(arguments(follow))
        .status()
        .map_err(|error| {
            format!(
                "`{} {}` could not be run ({error}): this command reads the server's log from journald, which is \
                 what the systemd unit writes",
                tools.journalctl.display(),
                arguments(follow).join(" ")
            )
        })?;
    Ok(status.code().unwrap_or(-1))
}

/// One session's log: the file, printed, and followed when `follow` is set.
///
/// The id is checked before it becomes a path component — the same rule the API's own route applies
/// ([`is_id`]) — because the reason this command has for refusing an id is not "no such file" but "that is not a
/// session id".
pub fn session(
    id: &str,
    following: bool,
    config: &Config,
    out: &mut impl Write,
) -> Result<(), String> {
    if !is_id(id) {
        return Err(format!(
            "{id:?} is not a session id (sixteen lowercase hexadecimal characters), so there is no session whose \
             log this could be"
        ));
    }
    let path = SessionLog::new(config.data_root.clone()).path(id);
    if !path.is_file() {
        return Err(format!(
            "{} is not there: this machine has no log for session {id} — a session writes one when its container \
             ends, with the launch line and the stderr's tail",
            path.display()
        ));
    }
    let written = if following {
        follow(&path, out, TICK, || true)
    } else {
        // One pass: the same reader with a stop condition that is already true, so "print the file" and "print the
        // file and then what arrives" are one loop instead of two spellings of reading a file.
        follow(&path, out, TICK, || false)
    };
    written.map_err(|error| format!("{} could not be read ({error})", path.display()))
}

/// Print what `path` holds, and then — while `keep_going` says so — whatever is appended to it.
///
/// The loop is the whole of "follow": read to end of file, write what was read, and ask again after `tick` until
/// the caller says stop. It is a function of its own rather than a `while true` inside [`session`], because that
/// is what makes "a line appended after the first read is printed" a test with no clock, no child and no file
/// watcher in it.
///
/// Reading stops at end of file rather than waiting for a change, and the position is the file handle's: **a file
/// that shrinks is not re-read** and a file replaced by a new inode is not followed at all. That is deliberate and it
/// is a limit, not a claim — `session.log` is append-only (`Exits::record` only appends) and a session's directory is
/// removed rather than rewritten, so neither case is one this build produces. A follower that seeks back on a size
/// change would have to decide what "the same log" means; this command does not, and says so.
pub fn follow(
    path: &Path,
    out: &mut impl Write,
    tick: Duration,
    mut keep_going: impl FnMut() -> bool,
) -> io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let mut wrote = false;
        loop {
            match file.read(&mut buffer)? {
                0 => break,
                read => {
                    out.write_all(&buffer[..read])?;
                    wrote = true;
                }
            }
        }
        if wrote {
            out.flush()?;
        }
        if !keep_going() {
            return Ok(());
        }
        std::thread::sleep(tick);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A temp file that removes itself.
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(label: &str, body: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("holosmith-logs-{label}-{}.log", std::process::id()));
            std::fs::write(&path, body).expect("the temp log is writable");
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn the_child_is_journalctl_for_the_unit_and_follows_only_when_asked() {
        assert_eq!(arguments(false), ["-u", "holosmith.service"]);
        assert_eq!(arguments(true), ["-u", "holosmith.service", "-f"]);
    }

    #[test]
    fn one_pass_prints_the_file_and_stops() {
        let file = TempFile::new("one-pass", "launch: pi --mode rpc\nexit 0\n");
        let mut out = Vec::new();
        follow(&file.0, &mut out, Duration::from_millis(1), || false).expect("the file reads");
        assert_eq!(
            String::from_utf8_lossy(&out),
            "launch: pi --mode rpc\nexit 0\n"
        );
    }

    #[test]
    fn a_line_appended_after_the_first_read_is_printed() {
        // The second call is what appends: the first read has already happened, so what the test observes can only
        // come from a re-read. A loop that read once and returned would print the first line alone — which is the
        // mutation this test exists for.
        let file = TempFile::new("follow", "first\n");
        let path = file.0.clone();
        let mut calls = 0;
        let mut out = Vec::new();
        follow(&path, &mut out, Duration::from_millis(1), || {
            calls += 1;
            if calls == 1 {
                let mut handle = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .expect("the temp log is appendable");
                handle.write_all(b"second\n").expect("the append lands");
            }
            calls < 3
        })
        .expect("the file reads");
        assert_eq!(String::from_utf8_lossy(&out), "first\nsecond\n");
    }

    #[test]
    fn an_id_that_is_not_a_session_id_is_refused_before_it_becomes_a_path() {
        let mut out = Vec::new();
        let sentence = session("../../etc/passwd", false, &config(), &mut out)
            .expect_err("a traversal is not an id");
        assert!(sentence.contains("not a session id"), "{sentence}");
        assert!(out.is_empty());
    }

    #[test]
    fn an_id_with_no_log_names_the_path_it_looked_at() {
        let config = config();
        let mut out = Vec::new();
        let sentence = session("0123456789abcdef", false, &config, &mut out)
            .expect_err("no log was written for this session");
        assert!(sentence.contains("0123456789abcdef"), "{sentence}");
        assert!(sentence.contains("session.log"), "{sentence}");
    }

    #[test]
    fn a_session_log_is_printed_as_written() {
        let config = config();
        let log = SessionLog::new(config.data_root.clone());
        log.append("0123456789abcdef", "launch: pi --mode rpc");
        log.append("0123456789abcdef", "exit 0");
        let mut out = Vec::new();
        session("0123456789abcdef", false, &config, &mut out).expect("the log reads");
        assert_eq!(
            String::from_utf8_lossy(&out),
            "launch: pi --mode rpc\nexit 0\n"
        );
    }

    fn config() -> Config {
        let root = std::env::temp_dir().join(format!("holosmith-logs-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Config {
            data_root: root,
            ..Config::default()
        }
    }
}
