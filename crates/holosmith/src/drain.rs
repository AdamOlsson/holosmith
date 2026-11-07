//! The drain: refusing new work until the machine is quiet enough to restart (HS-051, spec item 10 step 3).
//!
//! Item 10's promotion runs `drain` between the smoke test and `systemctl restart`, so that an upgrade does
//! not kill a turn somebody is watching. The drain is therefore **not** a shutdown: it flips one flag, waits
//! for the turns that are already running, stops the sessions that are merely idle, and then says so — while
//! `serve` keeps answering reads, exactly as item 7's 202s keep answering while work goes on.
//!
//! # The six things this module is
//!
//! 1. [`Control`] — the one bit the whole process reads: *this machine is draining*. It is the process's and
//!    not a row's, because a row would survive the restart the drain exists to precede and lie about a state
//!    that boot has to be free of. Handlers hold the `AppState` clone, so they hold this.
//! 2. [`Request`] — the socket's one request line, and the refusal a line that is not one answers.
//! 3. [`Drain`] — the sequence: set the flag, wait for `running` to empty, stop every `idle` session with
//!    [`SERVER_RESTART`], report. One value, built by [`crate::http::AppState::drainer`], so the four handles
//!    it needs are wired in one place.
//! 4. [`listen`] — the unix socket, which is the whole of "not reachable over HTTP" (the API table's own
//!    words). It is bound by `serve` beside the two policies and aborted with them.
//! 5. [`serve`] — one task per connection: read one line, answer with progress lines and one terminal
//!    `result:` line.
//! 6. [`socket_path`] — `<admin dir>/drain.sock`, the one spelling both sides use ([`crate::deploy::ADMIN_DIR`]
//!    on a machine; `--admin-dir` for a run that is not the machine's).
//!
//! # Why a socket and not an endpoint
//!
//! Item 10 step 3 says "over a local unix socket" and the API table says "not reachable over HTTP". That is
//! one decision with one consequence: no route, no guard, no CORS layer, no `App` — so a phone that can reach
//! the control origin still cannot drain the machine, and the only caller is a process that can open a file in
//! `/run/holosmith`. The directory is step 2's, created 0700 and chowned to the service user (HS-049), which
//! is why the socket needs no authentication of its own: being able to open it *is* the authorisation.
//!
//! # Why the wait is a poll over the rows
//!
//! "Waits for running turns" has two candidate readers: the in-process turn tasks and the `sessions` rows. The
//! row is the honest one — a turn ends through [`crate::turn`] or [`crate::ladder`], both of which move the row
//! first, and a row is what a crash also leaves behind. A count of tasks would answer for this process only,
//! and would answer "no turns" for a machine whose database says otherwise.
//!
//! # What the drain is not
//!
//! It is not [`crate::serve`]'s process shutdown (SIGTERM → stop accepting → a bounded grace → exit): a
//! successful drain leaves the process serving reads and refusing work, and the restart is somebody else's
//! (HS-078's `upgrade`). It is not the idle stop ([`crate::sweeper`], one policy on a timer) and not the
//! eviction ([`crate::http::sessions`], inside a create): it is the fourth policy that stops something, and
//! the only one that is asked for by an operator rather than by a rule.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

use crate::db::Database;
use crate::events::EventWriter;
use crate::sessions::Session;
use crate::sessions::state::{StateError, Transitioner, Trigger};
use crate::stop::Stopper;

/// The reason a session the drain stopped records (spec item 10 step 3).
///
/// The fourth reason word a policy writes — `forced_abort`, `deleted` and `evicted` (with `idle_stop` and the
/// turn's two) are the others — and the one that tells "the machine was going down" from "it stopped to make
/// room", "it sat idle" and "I stopped it". It lives here rather than in the `sessions` module because every
/// reason word belongs to the policy that produces it (ADR-0038's rule, spelled the same way in
/// [`crate::sweeper::IDLE_STOP`]).
pub const SERVER_RESTART: &str = "server_restart";

/// The socket's name inside the admin directory.
///
/// One constant, because two spellings of a socket path is a drain nobody can connect to. The directory is
/// [`crate::deploy::ADMIN_DIR`]'s; this is only what the drain puts in it.
pub const SOCKET_NAME: &str = "drain.sock";

/// How often the wait looks at the rows.
///
/// A hundred milliseconds: the drain is measured in minutes and the turn it waits for ends in seconds, so
/// anything coarser would make a fast turn look slow and anything finer would be a lock acquisition nobody
/// reads.
pub const POLL: Duration = Duration::from_millis(100);

/// How long a connection may take to say one line before it is dropped.
///
/// The drain is a local, hand-driven command; a connection that says nothing is a stuck client, not work
/// worth waiting for.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The most the socket reads from a connection, newline included. `drain 600000` is 12 bytes; the bound
/// exists so a client cannot make the server allocate, and a line that reaches it without a newline is refused
/// rather than parsed (`drain 12345…` truncated would otherwise be a smaller, valid wait).
pub const MAX_REQUEST: usize = 64;

/// The path of the drain socket inside one admin directory.
///
/// The one spelling both sides use: `serve` binds it and `holosmith drain` connects to it, so a machine's
/// `/run/holosmith/drain.sock` and a run's `--admin-dir` tree are the same arithmetic in one place.
pub fn socket_path(admin_dir: &Path) -> PathBuf {
    admin_dir.join(SOCKET_NAME)
}

/// The process's draining bit (HS-051).
///
/// `Clone` because it is an `Arc` and every handler holds one through [`crate::http::AppState`]: the flag is
/// read on the request path and set on the socket's task, and those are different tasks by construction.
#[derive(Debug, Clone, Default)]
pub struct Control {
    draining: Arc<AtomicBool>,
}

impl Control {
    /// A machine that is not draining — every boot, and every test that does not ask for one.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the bit, and answer whether this call is the one that set it.
    ///
    /// `true` means "the drain started here", which is the one difference the report makes between `draining`
    /// and `already draining` — the sequence after it is the same either way, so a second drain is a join
    /// rather than a refusal (the operator reaching for the same tool twice is not an error).
    pub fn begin(&self) -> bool {
        !self.draining.swap(true, Ordering::SeqCst)
    }

    /// Whether new work is refused. The one question a handler asks; a plain load, so a request path can ask it
    /// as often as it likes.
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }
}

/// The socket's request: one verb and one number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Drain, waiting at most this long for the turns that are running.
    Drain(Duration),
}

impl Request {
    /// Read one request line, or the sentence a line that is not one answers.
    ///
    /// The grammar is deliberately tiny: `drain <millis>`. Milliseconds rather than a unit suffix because the
    /// client has already parsed the operator's `--wait 10m` with the config's own parser
    /// ([`crate::config::Duration`]), and a second duration grammar on the wire is a second thing to get wrong.
    /// Anything else — a `status` verb an earlier draft had, an HTTP request line written here by mistake — is
    /// refused with a sentence rather than a guess.
    pub fn parse(line: &str) -> Result<Self, &'static str> {
        let line = line.trim_matches(['\r', '\n', ' ', '\t']);
        // A line with no space is one word, which is a verb with no argument rather than a malformed
        // request — the two sentences below then say which mistake it was.
        let (verb, argument) = line.split_once(' ').unwrap_or((line, ""));
        if verb != "drain" {
            return Err("the only request this socket answers is `drain <milliseconds>`");
        }
        let millis: u64 = argument
            .trim()
            .parse()
            .map_err(|_| "the drain's argument must be a whole number of milliseconds")?;
        Ok(Self::Drain(Duration::from_millis(millis)))
    }
}

/// How the drain ended, which is the one word the client turns into an exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// Every running turn finished and every idle session was stopped.
    Drained,
    /// The `--wait` deadline passed with a turn still running. The drain stays on.
    Expired,
    /// The request could not be acted on.
    Refused,
}

impl Ending {
    /// The word the terminal `result:` line carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Drained => "drained",
            Self::Expired => "expired",
            Self::Refused => "refused",
        }
    }

    /// The word back to an ending, for the client. `None` is a word this build does not know, which the client
    /// treats as a failure rather than as success.
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "drained" => Some(Self::Drained),
            "expired" => Some(Self::Expired),
            "refused" => Some(Self::Refused),
            _ => None,
        }
    }

    /// Whether this ending means the operator got what they asked for, which is the exit code.
    ///
    /// `Expired` is deliberately not: acceptance bullet 4 says the wait's expiry "returns non-zero and leaves
    /// the drain state active rather than silently succeeding".
    pub fn succeeded(self) -> bool {
        matches!(self, Self::Drained)
    }
}

/// The drain's sequence: the flag, the wait, the stop and the report (HS-051).
///
/// `Clone` for [`crate::sweeper::Sweeper`]'s reason: `serve` builds one and hands it to the socket's accept
/// loop, which clones it per connection — the fields are an `Arc`, a sender and two `Clone` handles, so a
/// connection owns its own copy and two drains in flight cannot share a sequence.
#[derive(Debug, Clone)]
pub struct Drain {
    control: Control,
    database: Arc<Mutex<Database>>,
    stopper: Stopper,
    events: EventWriter,
}

impl Drain {
    pub fn new(
        control: Control,
        database: Arc<Mutex<Database>>,
        stopper: Stopper,
        events: EventWriter,
    ) -> Self {
        Self {
            control,
            database,
            stopper,
            events,
        }
    }

    /// Run one drain, reporting each fact as it happens.
    ///
    /// The order is the acceptance's third bullet and it is not interchangeable: **set the flag** (so a create
    /// that arrives now is refused rather than racing the wait), **wait for the turns** (a turn that is already
    /// running is the thing the drain exists to protect), **then stop the idle sessions** (they cost nothing
    /// but a container each, and stopping them before the wait would leave the running turn's settle without a
    /// session to settle into — `running → idle` needs a row that is not yet `stopped`).
    ///
    /// `report` is called once per line, in order. It is a callback rather than a returned `Vec` because a
    /// drain that waits a minute must say so *while* it waits — a client that printed nothing until the end
    /// would be indistinguishable from one that hung.
    pub async fn run(&self, wait: Duration, report: &LineSink) -> Ending {
        let started = self.control.begin();
        report.send(if started {
            "state: draining"
        } else {
            "state: already draining"
        });
        // A duration so large the monotonic clock cannot add it is not a wait this command can honour, and
        // refusing it is better than a panic in an `Instant` addition or a wait with no end.
        let Some(deadline) = tokio::time::Instant::now().checked_add(wait) else {
            report.send("note: the wait is too long for this machine's clock, so nothing was done");
            report.result(Ending::Refused);
            return Ending::Refused;
        };
        match self.wait_for_turns(deadline, report).await {
            Wait::Quiet => {}
            Wait::Expired => {
                report.send(&format!(
                    "note: the wait ran out after {} with a turn still running; this machine is still draining",
                    describe(wait)
                ));
                report.result(Ending::Expired);
                return Ending::Expired;
            }
            // The wait said what it could not do; this is the one terminal line every sequence owes its client,
            // because the client takes the **last** `result:` and a report without one is a connection that
            // looks like it died mid-drain.
            Wait::Unreadable => {
                report.result(Ending::Refused);
                return Ending::Refused;
            }
        }
        match self.stop_idle().await {
            Ok(stopped) => {
                report.send(&format!(
                    "stopped: {} with reason {SERVER_RESTART}",
                    plural(stopped, "session")
                ));
                report.result(Ending::Drained);
                Ending::Drained
            }
            // The rows could not be read or could not be moved, so this machine is not quiet and 0 would say it
            // was. No count is reported: there is no honest number to give.
            Err(()) => {
                report.result(Ending::Refused);
                Ending::Refused
            }
        }
    }

    /// Wait until no row is `running`, or the deadline passes.
    ///
    /// The answer is three-valued because the two failures are not the same fact: a turn that outlasted the wait
    /// leaves the drain **on and honest** (`Expired`), while a database that cannot be read leaves this command
    /// unable to say what the machine is doing (`Unreadable`, which the caller turns into `refused`). Reporting the
    /// second as the first would print "the wait ran out with a turn still running" over a machine whose turns
    /// nobody counted.
    ///
    /// `starting` is deliberately **not** waited for: the table has one `Drain` row and it leaves `starting`,
    /// so a provisioning session cannot be stopped by this policy and must not be lied about. It comes back as
    /// `idle` through the startup reconcile (ADR-0053), which is item 10 step 6's own sentence — so it is
    /// reported once and left alone.
    async fn wait_for_turns(&self, deadline: tokio::time::Instant, report: &LineSink) -> Wait {
        let mut reported = None;
        let mut noted_starting = false;
        loop {
            let counts = match self.counts() {
                Ok(counts) => counts,
                // A database that cannot be read is a machine this command cannot describe. Reporting it as
                // "expired" would say the wait ran out, which is not what happened, so the report says what it
                // could not do and the client exits non-zero.
                Err(error) => {
                    report.send(&format!("note: the sessions could not be read ({error})"));
                    return Wait::Unreadable;
                }
            };
            if reported != Some(counts.running) && counts.running > 0 {
                report.send(&format!(
                    "waiting: {} still running",
                    plural(counts.running, "turn")
                ));
                reported = Some(counts.running);
            }
            if !noted_starting && counts.starting > 0 {
                report.send(&format!(
                    "note: {} starting and left alone, and comes back as idle after the restart",
                    plural(counts.starting, "session is")
                ));
                noted_starting = true;
            }
            if counts.running == 0 {
                return Wait::Quiet;
            }
            if tokio::time::Instant::now() >= deadline {
                return Wait::Expired;
            }
            // Sleep to the poll or to the deadline, whichever is sooner, so a short `--wait` expires when it
            // said it would rather than a poll after it.
            let next = tokio::time::Instant::now() + POLL;
            tokio::time::sleep_until(next.min(deadline)).await;
        }
    }

    /// Stop every `idle` session with [`SERVER_RESTART`], and answer how many were moved.
    ///
    /// The row moves first and the container's stop goes to a task of its own — ADR-0020's order, ADR-0024's
    /// shape and the sweeper's: the row is what the next boot reads, so a wedged `docker stop` must not hold
    /// this command up. `Stopper` writes spec item 6's flag before the call, so the exit this produces is one
    /// we asked for rather than a crash (ADR-0038).
    ///
    /// `Err` means the rows could not be read or could not be moved, which is **not** the same fact as
    /// "there was nothing to stop": the caller turns it into `refused` and a non-zero exit, because a machine
    /// whose rows this command could not read is not a machine it may call quiet.
    async fn stop_idle(&self) -> Result<u64, ()> {
        let candidates = match self.idle() {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::error!(
                    target: "holosmith::drain",
                    %error,
                    "the drain could not read the sessions it has to stop"
                );
                return Err(());
            }
        };
        let mut stopped = 0_u64;
        for session in candidates {
            match self
                .transitioner()
                .apply(&session.id, Trigger::Drain, Some(SERVER_RESTART))
                .await
            {
                Ok(_) => {}
                // The row is `stopped` and only the report of it was lost (a process on its way down), so the
                // container must still be stopped.
                Err(StateError::Unreported(_)) => {}
                // An ordinary race, not a failure: the session took a prompt between the read and the move (which
                // the flag now prevents, so this is the millisecond before it), or somebody deleted it. The
                // operator's restart is theirs to time, and this drain said it was quiet when it looked.
                Err(error @ (StateError::Illegal { .. } | StateError::UnknownSession(_))) => {
                    tracing::info!(
                        target: "holosmith::drain",
                        session = %session.id,
                        %error,
                        "an idle session left idle before the drain stopped it"
                    );
                    continue;
                }
                // The row could not be written: this machine is not quiet and the count would be a lie.
                Err(error) => {
                    tracing::error!(
                        target: "holosmith::drain",
                        session = %session.id,
                        %error,
                        "the drain could not stop a session"
                    );
                    return Err(());
                }
            }
            stopped += 1;
            tracing::info!(
                target: "holosmith::drain",
                session = %session.id,
                "the session was stopped for a restart"
            );
            self.spawn_stop(session);
        }
        Ok(stopped)
    }

    /// The container stop, off this command's path (ADR-0024's reason, the sweeper's shape).
    fn spawn_stop(&self, session: Session) {
        let stopping = self.stopper.clone();
        tokio::spawn(async move {
            if let Err(error) = stopping.stop(&session).await {
                tracing::warn!(
                    target: "holosmith::drain",
                    session = %session.id,
                    %error,
                    "the backend did not stop; the row is still stopped"
                );
            }
        });
    }

    /// The rows a drain stops: every session that is `idle` right now.
    fn idle(&self) -> Result<Vec<Session>, crate::sessions::RepoError> {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        database
            .sessions()
            .list(Some(crate::sessions::Status::Idle))
    }

    /// How many rows are in the two states the wait cares about.
    fn counts(&self) -> Result<Counts, crate::sessions::RepoError> {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        let counts = database.sessions().counts_by_status()?;
        let mut tally = Counts::default();
        for (status, count) in counts {
            match status {
                crate::sessions::Status::Running => tally.running = count,
                crate::sessions::Status::Starting => tally.starting = count,
                _ => {}
            }
        }
        Ok(tally)
    }

    /// The one state function, wired to this drain's two handles.
    fn transitioner(&self) -> Transitioner {
        Transitioner::new(Arc::clone(&self.database), self.events.clone())
    }
}

/// Where a drain's lines go: the connection's writer, or a test's `Vec`.
///
/// A channel rather than a callback because the waits are `async`: a synchronous `FnMut` could only buffer the
/// progress lines and hand them over at the end, and a client that printed nothing until a minute-long wait was
/// over would be indistinguishable from one that hung.
#[derive(Debug, Clone)]
pub struct LineSink(tokio::sync::mpsc::UnboundedSender<String>);

impl LineSink {
    /// A sink and the receiver a connection's writer task reads.
    pub fn new() -> (Self, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Self(sender), receiver)
    }

    /// Send one line of the report. A receiver that has gone means the client hung up: the line is dropped
    /// and the sequence carries on, because the state change is the drain and the report is only its account.
    pub fn send(&self, line: &str) {
        let _ = self.0.send(line.to_string());
    }

    /// The terminal line, which is the one a client turns into an exit code.
    pub fn result(&self, ending: Ending) {
        self.send(&format!("result: {}", ending.as_str()));
    }
}

/// How the wait ended, which is three facts and not two.
///
/// `Expired` and `Unreadable` are both "not quiet", and they are the two answers that must never be printed as
/// each other: the first leaves the drain on and reports the turn it waited for, the second cannot report a turn
/// at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// No row is `running`: the machine is quiet enough to stop the idle sessions.
    Quiet,
    /// The deadline passed with a turn still running. The drain stays on.
    Expired,
    /// The rows could not be read, so nothing about this machine can be said.
    Unreadable,
}

/// The two counts the wait reads.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Counts {
    running: u64,
    starting: u64,
}

/// `<n> turn` / `<n> turns`, so a report line never says "1 turns".
fn plural(count: u64, noun: &str) -> String {
    // The nouns this command uses are `turn` and `session is`, whose plurals are `turns` and `sessions are`:
    // one suffix rule, no table.
    if count == 1 {
        format!("{count} {noun}")
    } else if noun.ends_with(" is") {
        format!("{count} {} are", noun.trim_end_matches(" is"))
    } else {
        format!("{count} {noun}s")
    }
}

/// A duration as the operator wrote it (`1s`, `200ms`), for the sentence a report line carries.
fn describe(wait: Duration) -> String {
    crate::config::Duration::from_millis(wait.as_millis() as u64).to_string()
}

/// Bind the admin socket, creating its directory if this is the first run on the machine.
///
/// Three facts, each a measurement this project has paid for:
///
/// - **The directory is step 2's, and a `serve` that is not the machine's has to make its own.** On a deployed
///   host `/run/holosmith` exists, 0700, chowned to the service user (HS-049) — a tmpfs entry a reboot empties,
///   which is why the deploy re-creates it. A hand-run `serve` (`--admin-dir`, a dev instance, a gate case) may
///   be the first thing on a tree that has never been deployed, so it is created here, 0700, and only when it
///   is absent: an operator's own mode is theirs (the data root's rule).
/// - **A socket left behind by a killed process must not wedge the next boot.** `UnixListener::bind` fails with
///   `AddrInUse` for a file, live or dead, so the path is probed first: a connection that is *refused* is a dead
///   server's socket and is unlinked; one that succeeds means another server owns this machine's admin socket,
///   and unlinking it would hand its clients to us.
/// - **Anything else is not ours to remove.** A regular file at the socket's path is an operator's, and the
///   answer is a sentence naming it rather than a delete.
pub fn listen(admin_dir: &Path) -> Result<UnixListener, String> {
    prepare(admin_dir)?;
    let path = socket_path(admin_dir);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "{} could not be read ({error}), so this run does not replace whatever is there",
                path.display()
            ));
        }
        Ok(metadata) => {
            use std::os::unix::fs::FileTypeExt as _;
            // `connect` answers `ECONNREFUSED` for a *regular file* as well as for a dead server's socket
            // (measured), so the file's own type decides before anything is removed: a socket is probed, and
            // anything else is an operator's file this run has no business deleting.
            if !metadata.file_type().is_socket() {
                return Err(format!(
                    "{} exists and is not a socket, so this run leaves it alone",
                    path.display()
                ));
            }
            match std::os::unix::net::UnixStream::connect(&path) {
                Ok(_) => {
                    return Err(format!(
                        "{} is already answered by a running server, which owns this machine's admin socket",
                        path.display()
                    ));
                }
                // A dead server's socket file. Removing it is the only way to bind, and it names nothing else.
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(&path).map_err(|error| {
                        format!(
                            "the stale drain socket {} could not be removed ({error})",
                            path.display()
                        )
                    })?;
                }
                Err(error) => {
                    return Err(format!(
                        "{} exists and could not be probed ({error}), so this run does not replace it",
                        path.display()
                    ));
                }
            }
        }
    }
    let listener = UnixListener::bind(&path).map_err(|error| {
        format!(
            "the drain socket {} could not be bound ({error})",
            path.display()
        )
    })?;
    // The socket is the service user's: the directory is 0700, and the file's own mode says the same thing for
    // anyone who can already see it. A failure is one `warn` and not a refusal — a socket whose mode could not
    // be narrowed is still a socket the directory protects.
    if let Err(error) =
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
    {
        tracing::warn!(
            target: "holosmith::drain",
            path = %path.display(),
            %error,
            "the drain socket's mode could not be set to 0600; its directory is still 0700"
        );
    }
    tracing::debug!(
        target: "holosmith::drain",
        path = %path.display(),
        "the admin socket is listening"
    );
    Ok(listener)
}

/// Create the admin directory when it is absent, 0700, and leave an existing one's mode alone.
fn prepare(admin_dir: &Path) -> Result<(), String> {
    if admin_dir.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(admin_dir).map_err(|error| {
        format!(
            "the admin directory {} could not be created ({error}); `server deploy`'s step 2 creates it for \
             the service user",
            admin_dir.display()
        )
    })?;
    std::fs::set_permissions(
        admin_dir,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .map_err(|error| {
        format!(
            "the admin directory {} could not be set to 0700 ({error})",
            admin_dir.display()
        )
    })
}

/// Accept connections until the task is aborted, one task per connection.
///
/// A connection's failure is its own: a drain that could not be completed is reported to *that* client and
/// nothing else, because the next one may well succeed. The accept loop itself does not stop for a failed
/// accept either — an `EMFILE` or a connection reset between `accept` and the task is not a reason to leave the
/// machine with no admin socket at all.
pub fn spawn(listener: UnixListener, drain: Drain) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _address)) => {
                    let drain = drain.clone();
                    tokio::spawn(async move {
                        if let Err(error) = answer(stream, drain).await {
                            tracing::debug!(
                                target: "holosmith::drain",
                                %error,
                                "a drain connection ended without an answer"
                            );
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        target: "holosmith::drain",
                        %error,
                        "the admin socket could not accept a connection"
                    );
                    // A loop that spun on a persistent error would burn a core; one tick is enough to make a
                    // transient one (a reset connection, a full descriptor table) harmless.
                    tokio::time::sleep(POLL).await;
                }
            }
        }
    })
}

/// One connection: read a request line, run it, write the lines as they arrive.
///
/// A write that fails is logged and **not** fatal: the client hung up, and the drain it asked for has already
/// set the flag. Abandoning the sequence there would leave a machine that refuses work and never stopped its
/// idle sessions, which is the worst of the three states; finishing it and losing the report is the best.
async fn answer(stream: UnixStream, drain: Drain) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let request = match read_request(reader).await {
        Ok(request) => request,
        Err(sentence) => {
            let _ = write_line(&mut writer, &format!("note: {sentence}")).await;
            return write_line(
                &mut writer,
                &format!("result: {}", Ending::Refused.as_str()),
            )
            .await;
        }
    };
    let Request::Drain(wait) = request;
    let (report, mut lines) = LineSink::new();
    let sequence = drain.run(wait, &report);
    tokio::pin!(sequence);
    let mut connected = true;
    loop {
        tokio::select! {
            line = lines.recv() => {
                // `report` is alive in this scope, so the channel is never closed under us; ending the loop
                // rather than spinning is still the honest answer if it ever were.
                let Some(line) = line else { break };
                if connected && let Err(error) = write_line(&mut writer, &line).await {
                    tracing::debug!(
                        target: "holosmith::drain",
                        %error,
                        "a drain client stopped reading; the drain carries on"
                    );
                    connected = false;
                }
            }
            ending = &mut sequence => {
                let _ = ending;
                break;
            }
        }
    }
    // Whatever the sequence sent between the last poll and its end: the `result:` line is always among it.
    while let Ok(line) = lines.try_recv() {
        if connected && let Err(error) = write_line(&mut writer, &line).await {
            tracing::debug!(
                target: "holosmith::drain",
                %error,
                "a drain client stopped reading after the sequence finished"
            );
            connected = false;
        }
    }
    Ok(())
}

/// Write one line and flush it, so the client sees it now rather than when the connection closes.
async fn write_line(writer: &mut OwnedWriteHalf, line: &str) -> io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

/// Read one request line under [`REQUEST_TIMEOUT`], bounded to [`MAX_REQUEST`] bytes.
async fn read_request(reader: tokio::net::unix::OwnedReadHalf) -> Result<Request, &'static str> {
    // `take` rather than a length check afterwards: a client that never sends a newline must not be able to
    // grow the buffer. A line that reaches the cap without a newline is a truncation, and a truncation of
    // `drain 600000` is a valid, shorter wait — so it is refused rather than parsed.
    let mut reader = BufReader::new(reader.take(MAX_REQUEST as u64));
    let mut line = String::new();
    match tokio::time::timeout(REQUEST_TIMEOUT, reader.read_line(&mut line)).await {
        Ok(Ok(0)) => Err("the connection said nothing, and this socket answers one request line"),
        Ok(Ok(read)) if read >= MAX_REQUEST && !line.ends_with('\n') => {
            Err("the request line is longer than this socket reads")
        }
        Ok(Ok(_)) => Request::parse(&line),
        Ok(Err(_)) => Err("the request line could not be read"),
        Err(_) => Err("the connection said nothing in time"),
    }
}
