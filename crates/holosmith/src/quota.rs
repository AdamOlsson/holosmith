//! The per-session disk soft cap: one session that hoards disk is stopped, so a bad build cannot starve the
//! others (HS-063, spec item 14).
//!
//! Item 14's second sentence is a policy, not an endpoint: "`session_disk_soft_gb` (default 10): a watchdog
//! runs `du` on each live session's worktree every 5 minutes. Over the limit it emits an `error` event with
//! code `disk_quota`, aborts the turn and stops the session (resumable after cleanup)." This module is the
//! watchdog, and it is the third of the build's periodic policies — after the idle stop (HS-021) and
//! compaction (HS-060) — with one thing the other two do not have: it has to **end a turn**, not only move a
//! row.
//!
//! Six rules it keeps:
//!
//! - **The candidate set is the repository's rows, filtered by a pure function.** [`watchable`] takes the
//!   `list(None)` answer the reconciler and the compactor already read: `running` and `idle` rows, and only
//!   those whose worktree is recorded. `starting` is not a candidate — a fresh row has no worktree to measure
//!   and a recovered one is being re-provisioned, so a pass over it would race the provisioner — and
//!   `stopped`, `error` and `deleting` belong to flows of their own. It is also why one crossing is one event:
//!   a row this policy stopped is no longer a candidate.
//! - **The row moves before the container, and the turn ends before the row.** A `running` session ends
//!   through the abort ladder's second rung ([`Ladder::end_turn`] — rung 1 asks the agent, rung 2 is the
//!   table's `abort_ladder` row with this policy's reason), and an `idle` one through the table's own
//!   `disk_quota` row. Both end in `stopped`; neither ends in `error`, which is the story's third acceptance
//!   bullet. `Ladder::climb` is deliberately **not** used: it resumes after a forced stop, and a session over
//!   its cap must stay stopped until the operator has cleaned up.
//! - **The container's stop is [`Stopper`]'s, and it is best effort.** Spec item 6's `expected_stop` flag is
//!   written before the call, so the exit this stop produces is classified as ours rather than as a crash
//!   (ADR-0038), and a stop that fails is a log line rather than a retry — the operator can delete a session
//!   whose container outlived it.
//! - **The measurement is not under the lock.** `du` of a multi-gigabyte tree is hundreds of milliseconds, so
//!   the rows are read, the guard is dropped, and each measurement runs on `spawn_blocking` — the rule every
//!   subprocess follows here (the compactor's per-session step may hold the mutex because its work is
//!   synchronous; this one may not).
//! - **A measurement that cannot run is not a pass.** A `du` that is missing, refuses, or prints something
//!   that is not a `du -sk` answer stops nothing and is a `warn` line: item 14's own rule, the one HS-062's
//!   `df` reads the same way.
//! - **It is not a state machine.** Every state change goes through
//!   [`Transitioner::apply`](crate::sessions::state::Transitioner::apply); a refused application means the
//!   session left the candidate set between the read and the move — an ordinary race with a prompt, a delete
//!   or a resume — which is an `info` line and not a bug, and the next tick is the retry.
//!
//! The tick is a constant rather than a config key for the sweeper's reason (ADR-0025): the spec's config list
//! is exhaustive, and the five minutes *is* the policy — [item 14] names the interval, where it names
//! `idle_stop_min` as a key. `session_disk_soft_gb = 0` is the off switch, like `idle_stop_min = 0`: no task
//! is built and no `du` runs ([`crate::http::AppState::quota`]).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::clock;
use crate::db::Database;
use crate::events::{EventWriter, NewEvent};
use crate::ladder::{Ending, Ladder};
use crate::sessions::state::{StateError, Transitioner, Trigger};
use crate::sessions::{Session, Status};
use crate::stop::Stopper;

/// The cap's one word: the reason a stopped row records, the table's trigger word, and the `code` on the
/// event.
///
/// One constant for three roles because they are one fact — "this session was stopped for its disk" — and
/// ADR-0008's rule is that a `code` is effectively an API, so a second spelling of it would be a second thing
/// a client has to learn. The trigger's word lives in the table beside every other trigger's; the test at the
/// bottom of this file is what keeps the two in step.
pub const DISK_QUOTA: &str = "disk_quota";

/// The program the measurement runs, named once.
///
/// [`crate::preflight::Tools::default`] starts its `du` from this, and production looks the same name up on
/// the same `PATH`: one spelling of the program, so a test's shim and a machine's utility cannot be two.
pub const DU: &str = "du";

/// How often each live session's worktree is measured (spec item 14's "every 5 minutes").
///
/// A constant, not a config key: the interval is the policy, and the spec's config list does not carry it.
/// It also bounds how late a stop can be, not when one happens.
pub const TICK: Duration = Duration::from_secs(5 * 60);

/// How long the first pass waits for the boot's own re-provisioning to settle.
///
/// The startup reconcile moves every live row to `starting` and restarts its provisioning (HS-044), so a pass
/// taken the instant this task spawns would find no candidate at all — and the first pass is exactly the one
/// that catches a session which was already over its cap when the machine restarted. So it waits until no row
/// is `starting`, and this is the bound that stops a wedged row from holding the policy off for ever.
const SETTLE: Duration = Duration::from_secs(60);

/// How often the first pass looks while it waits.
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// How many kibibytes a gibibyte is: the divisor that turns `du`'s answer into the whole number of GB every
/// sentence prints, and the multiplier that turns the config's whole GB back into a comparison.
///
/// The same arithmetic, and the same number, as [`crate::disk`]'s — a worktree's size and a filesystem's free
/// space are both measured in the kibibytes `du -k`/`df -k` print, and the key is whole GB in both places.
const KIB_PER_GIB: u64 = 1024 * 1024;

/// Why a worktree could not be measured.
///
/// Three arms rather than [`crate::disk::DiskError`]'s four: a worktree the sweep has a row for always exists
/// as a path, so "there is nothing to measure" is not an answer this reader can give. The arms are still the
/// ones a sentence's fix needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuError {
    /// `du` is not runnable at all — not installed, or not executable.
    Unrunnable(String),
    /// `du` ran and refused. Its own words are the fix's context, so they travel.
    Refused(String),
    /// `du` answered something that is not a `du -sk` answer.
    Unprintable(String),
}

impl std::fmt::Display for DuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unrunnable(error) => write!(f, "du is not runnable ({error})"),
            Self::Refused(said) => write!(f, "`du` refused the worktree ({said})"),
            Self::Unprintable(text) => {
                write!(f, "`du` printed {text:?}, which is not a du -sk answer")
            }
        }
    }
}

/// Measure `path` with `program`, in kibibytes: the blocking half of the policy.
///
/// `-s` is one line for the whole tree rather than one per directory, and `-k` is kibibytes whatever the
/// machine's default block size is — the unit [`over`] expects. **No `--one-file-system`**: the flag is
/// GNU-specific (`busybox du` has no such option), and the cap's whole point is that *everything* under the
/// worktree counts — `target/`, `node_modules/`, and a nested mount too, which is disk one session made and
/// nobody else can use. `--` ends the options so a worktree whose path starts with `-` is a path.
pub fn measure(program: &Path, path: &Path) -> Result<u64, DuError> {
    let output = Command::new(program)
        .arg("-sk")
        .arg("--")
        .arg(path)
        .output()
        .map_err(|error| DuError::Unrunnable(error.to_string()))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(DuError::Refused(said));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    kibibytes(&text).ok_or(DuError::Unprintable(text.trim().to_string()))
}

/// The number `du -sk` printed: the first field of the first line.
///
/// `du -sk` answers `<kib>\t<path>`, so the count is everything before the first whitespace. A `du` that
/// printed nothing, a word, or a negative number is not an answer — and neither is one whose figure does not
/// fit in 64 bits, which is not a worktree this machine can hold.
pub fn kibibytes(output: &str) -> Option<u64> {
    let first = output.lines().next()?.split_whitespace().next()?;
    first.parse::<u64>().ok()
}

/// Whether `size_kib` is over `limit_gb`, the comparison spec item 14 asks ("over the limit").
///
/// In 128 bits because the multiplier is the config's number: `session_disk_soft_gb` is a `u64` the config
/// layer accepts without a ceiling, and `limit * 1024 * 1024` would wrap for an absurd one — a wrapped bound
/// would turn "never" into "always". Strictly greater: a worktree exactly at its cap has not crossed it.
/// `limit_gb = 0` never reaches this function (a zero cap builds no task at all), so the division that would
/// make it "everything is over" has one home: `AppState::quota`.
pub fn over(size_kib: u64, limit_gb: u64) -> bool {
    u128::from(size_kib) > u128::from(limit_gb) * u128::from(KIB_PER_GIB)
}

/// The rows this pass may measure, in id order so a pass is repeatable.
///
/// `running` and `idle` are the two states that are working and have files: a `running` turn is what the cap
/// exists to abort, and an `idle` session's build tree is still on the disk. The worktree filter is the row's
/// own column — `Worktree::of` names it and the provisioner records it — so "each live session's worktree" is
/// a column read rather than a path this module invents.
pub fn watchable(rows: &[Session]) -> Vec<Session> {
    let mut watch: Vec<Session> = rows
        .iter()
        .filter(|row| {
            matches!(row.status, Status::Running | Status::Idle) && !row.worktree.is_empty()
        })
        .cloned()
        .collect();
    watch.sort_by(|left, right| left.id.cmp(&right.id));
    watch
}

/// The per-session disk cap's watchdog: the ladder a running turn ends through, the stopper an idle session's
/// container goes through, the rows, the writer, the program and the limit.
///
/// `Clone` for the same reason [`crate::sweeper::Sweeper`] is: `serve` builds one at startup and hands it to a
/// task, and the fields are handles and numbers rather than state.
#[derive(Debug, Clone)]
pub struct Quota {
    ladder: Ladder,
    stopper: Stopper,
    database: Arc<Mutex<Database>>,
    events: EventWriter,
    program: PathBuf,
    limit_gb: u64,
}

impl Quota {
    pub fn new(
        ladder: Ladder,
        stopper: Stopper,
        database: Arc<Mutex<Database>>,
        events: EventWriter,
        program: PathBuf,
        limit_gb: u64,
    ) -> Self {
        Self {
            ladder,
            stopper,
            database,
            events,
            program,
            limit_gb,
        }
    }

    /// Hand the tick loop to a background task and return its handle.
    ///
    /// The handle is what stops it: `serve` aborts it once the listener's drain is over, so the watchdog never
    /// outlives the process's own serving and no shutdown waits for a tick to come round.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    /// Wait for the boot's provisioning to settle, sweep, then sleep, until the task is aborted.
    ///
    /// The first sweep comes after [`Self::settle`] and not before it, and the reason is the startup
    /// reconcile's: every live row is `starting` when this task spawns, so an immediate pass would measure
    /// nothing — and the pass after a restart is the one that matters most, because a session can already be
    /// over its cap when the machine comes back.
    pub async fn run(&self) {
        self.settle().await;
        loop {
            self.sweep().await;
            tokio::time::sleep(TICK).await;
        }
    }

    /// Wait until no session is `starting`, or until [`SETTLE`] is spent.
    ///
    /// A row that is still provisioning has no complete worktree to measure and no turn to end: a pass over it
    /// would race the provisioner for the row and could stop a session on its way back. The bound is what makes
    /// a wedged row a delay rather than a policy that never runs.
    async fn settle(&self) {
        let deadline = Instant::now() + SETTLE;
        loop {
            match self.provisioning() {
                Ok(0) => return,
                Ok(_) => {}
                // A database that cannot be read is the sweep's own failure to report; waiting on it would
                // turn one error into sixty seconds of them.
                Err(_) => return,
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    target: "holosmith::quota",
                    settle_s = SETTLE.as_secs(),
                    "a session was still provisioning, so the disk cap's first pass ran anyway"
                );
                return;
            }
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }

    /// How many rows are `starting`, which is how many provisioners are walking a session.
    fn provisioning(&self) -> Result<usize, crate::sessions::RepoError> {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        let rows = database.sessions().list(Some(Status::Starting))?;
        Ok(rows.len())
    }

    /// One pass: measure every candidate's worktree and stop the sessions that are over the cap.
    ///
    /// Public so a test can await a pass instead of racing a spawned task; `spawn` is what production calls. A
    /// repository failure is logged and ends the pass: the next tick is the retry, and a sweep that retried
    /// inside itself would spin on a closed database.
    pub async fn sweep(&self) {
        let candidates = match self.candidates() {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::error!(
                    target: "holosmith::quota",
                    %error,
                    "the disk cap's candidates could not be read"
                );
                return;
            }
        };
        tracing::debug!(
            target: "holosmith::quota",
            candidates = candidates.len(),
            limit_gb = self.limit_gb,
            "swept for sessions over the disk cap"
        );
        for session in candidates {
            let Some(size_kib) = self.size_of(&session).await else {
                continue;
            };
            if !over(size_kib, self.limit_gb) {
                continue;
            }
            self.stop(session, size_kib).await;
        }
    }

    /// The candidates, quietest-first order irrelevant: this policy reads every row.
    fn candidates(&self) -> Result<Vec<Session>, crate::sessions::RepoError> {
        let rows = {
            let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
            database.sessions().list(None)?
        };
        Ok(watchable(&rows))
    }

    /// Measure one candidate's worktree off the runtime's own threads, or answer `None` — with the reason in
    /// the log — when `du` could not tell.
    ///
    /// A failure to measure stops nothing: a check that cannot run is not a pass (spec item 14, and HS-062's
    /// four arms read the same way). The row is left exactly as it was, and the next tick asks again.
    async fn size_of(&self, session: &Session) -> Option<u64> {
        let program = self.program.clone();
        let worktree = PathBuf::from(&session.worktree);
        match tokio::task::spawn_blocking(move || measure(&program, &worktree)).await {
            Ok(Ok(size_kib)) => Some(size_kib),
            Ok(Err(error)) => {
                tracing::warn!(
                    target: "holosmith::quota",
                    session = %session.id,
                    worktree = %session.worktree,
                    %error,
                    "the session's worktree could not be measured"
                );
                None
            }
            // The blocking task panicked: nothing about this session can be concluded from that.
            Err(error) => {
                tracing::warn!(
                    target: "holosmith::quota",
                    session = %session.id,
                    %error,
                    "the measurement task did not finish"
                );
                None
            }
        }
    }

    /// Stop one session that is over the cap, end its turn if it has one, and report it once.
    ///
    /// The two arms differ only in how the row reaches `stopped`: a `running` turn is the ladder's to end (rung
    /// 1 asks the agent, rung 2 forces it), and an `idle` session has no turn, so the table's `disk_quota` row
    /// takes it straight to `stopped`. A row that is neither — it left the candidate set between the read and
    /// this call — is an `info` line: it is a race with a prompt, a resume or a delete, not a bug.
    async fn stop(&self, session: Session, size_kib: u64) {
        // The row as it stands now, not as the pass read it: a turn that settled between the read and here
        // has to be stopped as an idle session, and a row that has gone has nothing to stop.
        let current = match self.row(&session.id) {
            Ok(Some(current)) => current,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(
                    target: "holosmith::quota",
                    session = %session.id,
                    %error,
                    "the session could not be read before it was stopped"
                );
                return;
            }
        };
        match current.status {
            Status::Running => {
                let ending = self.ladder.end_turn(&current, DISK_QUOTA).await;
                if ending == Ending::Forced {
                    // Rung 2 moved the row to `stopped` and stopped the container: the report is all that is
                    // left, and it happens for a forced stop as much as for an asked one.
                    self.report(&current, size_kib).await;
                    return;
                }
                // Rung 1 worked: the turn runner wrote `running -> idle` with `abort_completed`, so the rest of
                // this is the idle arm's. Anything else (a delete, a resume) is nobody's to stop here.
                match self.row(&session.id) {
                    Ok(Some(idle)) if idle.status == Status::Idle => {
                        self.stop_idle(idle, size_kib).await;
                    }
                    Ok(_) => tracing::info!(
                        target: "holosmith::quota",
                        session = %session.id,
                        "the session left the row before the cap could stop it"
                    ),
                    Err(error) => tracing::warn!(
                        target: "holosmith::quota",
                        session = %session.id,
                        %error,
                        "the session could not be read after the turn ended"
                    ),
                }
            }
            Status::Idle => self.stop_idle(current, size_kib).await,
            _ => tracing::info!(
                target: "holosmith::quota",
                session = %session.id,
                state = current.status.as_str(),
                "the session left the disk cap's candidates between the read and the stop"
            ),
        }
    }

    /// The idle arm: the row first, then the container, then the report.
    async fn stop_idle(&self, session: Session, size_kib: u64) {
        match self
            .transitioner()
            .apply(&session.id, Trigger::DiskQuota, Some(DISK_QUOTA))
            .await
        {
            Ok(_) => {}
            // The row is `stopped` and only the report of it was lost: the process is on its way down, and the
            // container must still be stopped.
            Err(StateError::Unreported(_)) => {}
            Err(error) => {
                tracing::info!(
                    target: "holosmith::quota",
                    session = %session.id,
                    limit_gb = self.limit_gb,
                    %error,
                    "the disk cap's stop did not happen"
                );
                return;
            }
        }
        tracing::info!(
            target: "holosmith::quota",
            session = %session.id,
            limit_gb = self.limit_gb,
            size_kib = size_kib,
            "the session's worktree is over the disk cap and was stopped"
        );
        // The row is `stopped`, which is what frees the slot, so the sweep must not wait for the container: one
        // wedged backend then delays nothing but its own stop (ADR-0024's order, the sweeper's shape).
        let stopping = self.stopper.clone();
        let victim = session.clone();
        tokio::spawn(async move {
            if let Err(error) = stopping.stop(&victim).await {
                tracing::warn!(
                    target: "holosmith::quota",
                    session = %victim.id,
                    %error,
                    "the backend did not stop; the row is still stopped"
                );
            }
        });
        self.report(&session, size_kib).await;
    }

    /// Write the one `error` event a crossing earns, after the row is `stopped`.
    ///
    /// The kind is a new one — the event list has no `error` — and the increment is additive: a client drops a
    /// kind it does not render (ADR-0018), and this build's own client renders it. `code` is the same word as
    /// the row's reason, so the log and the row cannot disagree; `size_kib` is the measured number and
    /// `limit_gb` the cap it crossed, because a client reading the log later has no config to consult.
    async fn report(&self, session: &Session, size_kib: u64) {
        let gb = size_kib as f64 / KIB_PER_GIB as f64;
        let payload = serde_json::json!({
            "code": DISK_QUOTA,
            "message": format!(
                "the session's worktree holds {gb:.1} GB, over the {} GB soft cap; clean it up or raise \
                 session_disk_soft_gb",
                self.limit_gb
            ),
            "size_kib": size_kib,
            "limit_gb": self.limit_gb,
        });
        let event = NewEvent {
            session_id: session.id.clone(),
            kind: crate::events::ERROR.to_string(),
            payload: payload.to_string(),
            created_at: clock::unix_now(),
        };
        if let Err(error) = self.events.append(event).await {
            tracing::warn!(
                target: "holosmith::quota",
                session = %session.id,
                %error,
                "the disk cap's event was not queued; the row is still stopped"
            );
        }
    }

    /// The row itself, as it stands now.
    fn row(&self, session_id: &str) -> Result<Option<Session>, crate::sessions::RepoError> {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        database.sessions().get(session_id)
    }

    /// The one state function, wired to this watchdog's two handles.
    fn transitioner(&self) -> Transitioner {
        Transitioner::new(Arc::clone(&self.database), self.events.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row with `status`, and a worktree when `worktree` is not empty.
    fn row(id: &str, status: Status, worktree: &str) -> Session {
        Session {
            id: id.to_string(),
            project: "holosmith".to_string(),
            model: "claude-sonnet-4.5".to_string(),
            thinking: "high".to_string(),
            worktree: worktree.to_string(),
            base_commit: None,
            status,
            status_reason: None,
            client_key: None,
            network: false,
            created_at: 1_760_000_000,
            last_active_at: 1_760_000_000,
            stopped_at: None,
            usage_in: 0,
            usage_out: 0,
        }
    }

    #[test]
    fn the_trigger_word_is_the_reason_word() {
        // One fact, two languages: the table's word and the constant a row's reason and an event's code are
        // spelled with. A drift here would make a client filter on a word the row never carries.
        assert_eq!(Trigger::DiskQuota.as_str(), DISK_QUOTA);
    }

    #[test]
    fn the_comparison_is_strict_and_counts_gibibytes() {
        let gib = KIB_PER_GIB;
        assert!(!over(0, 10));
        assert!(!over(10 * gib - 1, 10));
        // Exactly at the cap is not over it: "over the limit" is the spec's word.
        assert!(!over(10 * gib, 10));
        assert!(over(10 * gib + 1, 10));
        // The number a 15 GiB tree reports is over a 10 GiB cap, and a KiByte is not a GiB.
        assert!(over(15 * gib, 10));
        assert!(!over(gib, 10));
    }

    #[test]
    fn a_cap_so_large_that_the_multiplier_would_wrap_is_still_never_crossed() {
        // `u64` arithmetic would wrap here and answer "over" for every size; 128 bits answers the truth.
        let absurd = u64::MAX;
        assert!(!over(u64::MAX, absurd));
        assert!(!over(1, absurd));
    }

    #[test]
    fn the_measurement_reads_the_first_field_of_the_first_line() {
        assert_eq!(
            kibibytes("15728640\t/srv/worktrees/abc\n"),
            Some(15_728_640)
        );
        // GNU `du` separates with a space when the path is short enough that it does not align columns.
        assert_eq!(kibibytes("12 /tmp/x\n"), Some(12));
        assert_eq!(kibibytes("  4096   /tmp/x\n"), Some(4096));
        assert_eq!(kibibytes(""), None);
        assert_eq!(kibibytes("du: cannot access\n"), None);
        assert_eq!(kibibytes("-5\t/path\n"), None);
    }

    #[test]
    fn the_candidates_are_the_live_rows_that_have_a_worktree() {
        let rows = vec![
            row("a", Status::Idle, "/p/.worktrees/a"),
            row("b", Status::Running, "/p/.worktrees/b"),
            row("c", Status::Starting, "/p/.worktrees/c"),
            row("d", Status::Stopped, "/p/.worktrees/d"),
            row("e", Status::Error, "/p/.worktrees/e"),
            row("f", Status::Deleting, "/p/.worktrees/f"),
            // A row the provisioner has not reached yet: nothing to measure.
            row("g", Status::Idle, ""),
        ];
        let ids: Vec<String> = watchable(&rows).into_iter().map(|row| row.id).collect();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn a_measurement_that_cannot_run_is_not_a_pass() {
        // A program that is not there, one that refuses, and one whose answer is not a `du` answer: all three
        // are errors rather than a size, because a check that cannot run must not read as "under the cap".
        let missing = measure(Path::new("/nonexistent/du"), Path::new("/tmp"));
        assert!(matches!(missing, Err(DuError::Unrunnable(_))));
        let dir = std::env::temp_dir().join(format!("holosmith-quota-du-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let refusing = dir.join("refusing");
        std::fs::write(&refusing, "#!/bin/sh\necho 'du: nope' >&2\nexit 1\n").expect("a shim");
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&refusing, std::fs::Permissions::from_mode(0o755)).expect("exec");
        let refused = measure(&refusing, &dir);
        assert!(matches!(refused, Err(DuError::Refused(_))), "{refused:?}");
        let babbling = dir.join("babbling");
        std::fs::write(&babbling, "#!/bin/sh\necho 'not a number at all'\n").expect("a shim");
        std::fs::set_permissions(&babbling, std::fs::Permissions::from_mode(0o755)).expect("exec");
        let unprintable = measure(&babbling, &dir);
        assert!(
            matches!(unprintable, Err(DuError::Unprintable(_))),
            "{unprintable:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
