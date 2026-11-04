//! Exit classification: what "the container stopped" means for the row (HS-030, spec item 6).
//!
//! Spec item 6 replaces the earlier clean-exit-means-ended rule with three answers, and the middle one
//! is why the expected-stop flag exists:
//!
//! | The server had recorded a stop | The container's exit code | The session |
//! | --- | --- | --- |
//! | yes | any | `stopped` — the exit is the consequence of our own stop |
//! | no | 0 | `stopped`, with reason `pi_exit` — the agent finished and nothing else happened |
//! | no | anything else | `error`, with the last stderr lines as its reason |
//!
//! [`classify`] is that table as a **pure function** with no I/O, so the three cases (and the
//! flag-cleared one) are proved by unit tests rather than by driving a container. [`Exits::record`] is
//! what applies an answer: it consumes the flag, moves the row through the one state function, and keeps
//! the stderr in the session's log.
//!
//! **Where the observation comes from, and which story owns it.** This build has no process whose exit
//! anybody can see: the container runs detached on `sleep infinity` (ADR-0036) until HS-031 attaches pi's
//! stdio, and `--rm` takes the container away a moment after it stops, so there is nothing left to
//! inspect (ADR-0037). HS-031's attachment is therefore the caller of [`Exits::record`] — it has the exit
//! code and the stderr pipe — and HS-030 owns the decision, the flag and the record. That boundary is
//! recorded in ADR-0038 rather than papered over with a watcher (`docker events`, a `docker wait` race
//! against `--rm`) that HS-031 would delete.
//!
//! **`session.log` is the session's stderr and the server's own launch line** — and nothing else.
//! `<data>/sessions/<id>/session.log` is append-only and carries the lines the container printed, verbatim;
//! the row's `status_reason` carries only the last few, because a phone shows one line. Spec item 15's
//! "`session.log` holds stderr only" is what keeps a *tool's* output out of it (HS-033 gives each tool call
//! its own file) and what makes the file worth reading when a session dies; HS-031 adds the launch line
//! there, because "which command did we run" is the first question a failed launch asks and there is no row
//! column for it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::db::Database;
use crate::events::EventWriter;
use crate::sessions::Session;
use crate::sessions::state::{Transitioner, Trigger};

/// The reason a clean exit records (spec item 6).
///
/// The same word the trigger carries: a `pi_exit` transition's reason is `pi_exit`, the way an
/// `idle_stop` transition's is `idle_stop`.
pub const PI_EXIT: &str = "pi_exit";

/// How many of stderr's last non-empty lines a failure puts in the row's `status_reason`.
///
/// Five, because the row is read on a phone and the row is also what a list shows: enough to identify a
/// stack trace's tail or a provider's error, short enough to stay one paragraph. The whole of stderr goes
/// to [`session.log`](Exits) regardless.
pub const STDERR_LINES: usize = 5;

/// Where a session's files live under the data root: `<data>/sessions/<id>` (ADR-0011).
pub const SESSIONS: &str = "sessions";

/// The file stderr is kept in (spec item 15).
pub const LOG_FILE: &str = "session.log";

/// How a container's exit reads, once the expected-stop flag has been consulted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// The server had recorded that this stop was its own: a policy stopped the container, so the row
    /// already says why (and `stopped` is what it says).
    Expected,
    /// Code 0 and no flag: the agent ended, and nothing was wrong.
    Clean,
    /// Any other exit: the reason is the last of what it printed, and the row is `error`.
    Failed {
        /// The last [`STDERR_LINES`] non-empty lines, or a sentence naming the code when it printed
        /// nothing.
        reason: String,
    },
}

/// The classifier: spec item 6's three cases, as one pure function.
///
/// The flag wins over the code, because a stop the server asked for can end in any exit code — a killed
/// process exits `143`, a `docker stop` that timed out exits `137`, and pi flushing its last frame may
/// exit `0`. Reading the code first would call a policy stop a crash.
pub fn classify(expected_stop: bool, code: i32, stderr: &str) -> Exit {
    if expected_stop {
        return Exit::Expected;
    }
    if code == 0 {
        return Exit::Clean;
    }
    Exit::Failed {
        reason: tail(code, stderr),
    }
}

/// The last [`STDERR_LINES`] non-empty lines of `stderr`, joined by a newline.
///
/// Blank lines are dropped: they are what a progress renderer leaves behind, and a reason made of them
/// would say nothing on the one line a phone shows. Nothing printed at all still has to be explained, so
/// the answer names the exit code — an `error` row with an empty reason is the one shape an operator
/// cannot act on.
fn tail(code: i32, stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return format!("the container exited {code} and printed nothing");
    }
    lines[lines.len().saturating_sub(STDERR_LINES)..].join("\n")
}

/// The two handles an exit's consequences need: the row, the event log, and where a session's files are.
///
/// `Clone` because its production caller is a task that supervises a container's stdio (HS-031), and a
/// supervisor is spawned rather than borrowed.
#[derive(Debug, Clone)]
pub struct Exits {
    database: Arc<Mutex<Database>>,
    events: EventWriter,
    data_root: PathBuf,
}

impl Exits {
    /// The classifier for the machine whose data root is `data_root`.
    pub fn new(
        database: Arc<Mutex<Database>>,
        events: EventWriter,
        data_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            database,
            events,
            data_root: data_root.into(),
        }
    }

    /// Classify how `session`'s container exited, and record what that means.
    ///
    /// The flag is consumed here — read and cleared in one statement — because the answer it gives is
    /// about exactly this exit; a flag left set would call the *next* crash a stop we asked for.
    ///
    /// Every failure is logged and none of them is returned. The session's process has already stopped,
    /// so there is nothing left to fail *at*: the operator needs the row to say what happened, and a row
    /// write that failed is one `warn` line rather than an error that would hide the classification.
    pub async fn record(&self, session: &Session, code: i32, stderr: &str) {
        let expected = self.take_flag(&session.id);
        self.append_log(&session.id, stderr);
        match classify(expected, code, stderr) {
            Exit::Expected => tracing::debug!(
                target: "holosmith::exit",
                session = %session.id,
                code,
                "the container exited after the stop the server asked for"
            ),
            Exit::Clean => {
                self.transition(&session.id, Trigger::PiExit, Some(PI_EXIT))
                    .await;
            }
            Exit::Failed { reason } => {
                tracing::warn!(
                    target: "holosmith::exit",
                    session = %session.id,
                    code,
                    %reason,
                    "the container exited unexpectedly"
                );
                // `UnexpectedExit` is the table's row for "the container exited on its own"
                // (`starting|idle|running -> error`), and the reason is what it printed.
                self.transition(&session.id, Trigger::UnexpectedExit, Some(&reason))
                    .await;
            }
        }
    }

    /// Consume the expected-stop flag. A read that fails, and a row that is gone, both answer `false`:
    /// without the flag the exit is classified on its own merits, which is the loud direction (`error`
    /// rather than `stopped`) and the one an operator can still act on.
    fn take_flag(&self, session_id: &str) -> bool {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        match database.sessions().take_expected_stop(session_id) {
            Ok(flag) => flag,
            Err(error) => {
                tracing::warn!(
                    target: "holosmith::exit",
                    session = %session_id,
                    %error,
                    "the expected-stop flag could not be read; the exit is classified as unexpected"
                );
                false
            }
        }
    }

    /// Append `stderr` to the session's log, one line per line.
    ///
    /// The directory is made on the first write, because a session that dies early may never have had one
    /// (HS-031 seeds the agent directory when it starts a container). A failure to write is a `warn` and
    /// nothing more: the row's reason is the durable half of this, and the log is the complete one.
    fn append_log(&self, session_id: &str, stderr: &str) {
        if stderr.is_empty() {
            return;
        }
        // A guard, not a rule about ids: this is the one place in the crate that turns a string into a path
        // component, and a future caller with an id from somewhere other than a row must not be able to
        // write outside `<data>/sessions/`. Every id today is the repository's 16 lowercase hex (ADR-0011).
        if !is_path_segment(session_id) {
            tracing::warn!(
                target: "holosmith::exit",
                session = %session_id,
                "the session id is not a path segment, so nothing was logged"
            );
            return;
        }
        let path = self.log_path(session_id);
        let written = (|| -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            file.write_all(stderr.as_bytes())?;
            if !stderr.ends_with('\n') {
                file.write_all(b"\n")?;
            }
            Ok(())
        })();
        if let Err(error) = written {
            tracing::warn!(
                target: "holosmith::exit",
                session = %session_id,
                path = %path.display(),
                %error,
                "the session log could not be written"
            );
        }
    }

    /// `<data>/sessions/<id>/session.log`.
    fn log_path(&self, session_id: &str) -> PathBuf {
        session_dir(&self.data_root, session_id).join(LOG_FILE)
    }

    /// One state change through the one function that writes states (ADR-0016), with a failure logged:
    /// the row is what happened, and the event is the report of it.
    async fn transition(&self, session_id: &str, trigger: Trigger, reason: Option<&str>) {
        Transitioner::new(Arc::clone(&self.database), self.events.clone())
            .apply_logged(session_id, trigger, reason)
            .await;
    }
}

/// `<data_root>/sessions/<id>`: where a session's own files live.
///
/// `session_id` is a row's id — the repository's 16 lowercase hex (ADR-0011) — and nothing here checks it:
/// a caller that hands this an id from somewhere else has to make it a path segment itself, which is what
/// [`Exits`]'s writer refuses to do (see `append_log`).
pub fn session_dir(data_root: &Path, session_id: &str) -> PathBuf {
    data_root.join(SESSIONS).join(session_id)
}

/// Whether `text` can be one path component: no separators, no `.`/`..`, nothing that a filesystem reads as
/// more than a name. A guard against writing outside a session's directory, not a second spelling of what
/// an id is.
fn is_path_segment(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_the_server_asked_for_is_expected_whatever_the_code() {
        // The three codes a policy stop actually produces: a clean flush, SIGTERM's `143` and
        // `docker stop`'s kill after its timeout.
        for code in [0, 143, 137] {
            assert_eq!(classify(true, code, "anything"), Exit::Expected, "{code}");
        }
    }

    #[test]
    fn a_clean_exit_without_the_flag_is_the_agent_finishing() {
        assert_eq!(classify(false, 0, ""), Exit::Clean);
        // Whatever it printed while working is not a reason: the session did not fail.
        assert_eq!(classify(false, 0, "warning: slow\n"), Exit::Clean);
    }

    #[test]
    fn any_other_exit_is_a_failure_with_the_last_lines_it_printed() {
        let stderr = (1..=9)
            .map(|n| format!("line {n}"))
            .collect::<Vec<String>>()
            .join("\n");
        let Exit::Failed { reason } = classify(false, 3, &stderr) else {
            panic!("a non-zero code without the flag is a failure");
        };
        assert_eq!(
            reason, "line 5\nline 6\nline 7\nline 8\nline 9",
            "the reason is the last {STDERR_LINES} lines, in order"
        );
    }

    #[test]
    fn an_exit_with_nothing_to_show_still_says_what_happened() {
        // A reason of `""` is the one shape a phone cannot act on, and blank lines are not content.
        for stderr in ["", "\n", "  \n\t\n"] {
            let Exit::Failed { reason } = classify(false, 1, stderr) else {
                panic!("a non-zero code without the flag is a failure");
            };
            assert_eq!(
                reason, "the container exited 1 and printed nothing",
                "{stderr:?}"
            );
        }
    }

    #[test]
    fn the_flag_cleared_after_a_stop_no_longer_excuses_the_next_exit() {
        // The flag is consumed by the classification (`Exits::record`), so the same code means different
        // things before and after: this is the pure half of "the flag is taken once".
        assert_eq!(classify(true, 0, "boom"), Exit::Expected);
        assert_eq!(classify(false, 0, "boom"), Exit::Clean);
        assert!(matches!(classify(false, 1, "boom"), Exit::Failed { .. }));
    }

    #[test]
    fn only_a_bare_name_can_be_a_sessions_path_component() {
        // The ids the repository makes pass; anything that could escape `<data>/sessions/` does not.
        assert!(is_path_segment("0123456789abcdef"));
        for text in ["", ".", "..", "../..", "a/b", "/etc/passwd", "a b", "a\0b"] {
            assert!(
                !is_path_segment(text),
                "{text:?} must not be a path segment"
            );
        }
    }

    #[test]
    fn a_sessions_files_live_under_the_data_root() {
        assert_eq!(
            session_dir(Path::new("/var/lib/holosmith"), "0123456789abcdef"),
            Path::new("/var/lib/holosmith/sessions/0123456789abcdef")
        );
    }
}
