//! The stopped-session expiry: a session nobody came back to is removed, so the operator does not prune by
//! hand (spec item 15, decision D3).
//!
//! Item 15's retention bullet is a policy, not an endpoint: "a daily job deletes sessions that have been stopped
//! for more than 7 days (config `stopped_ttl_days`, default 7), using the delete flow from item 9. A snapshot
//! runs first; if it fails, the session is skipped and retried the next day. Sessions in error are never
//! auto-deleted." This module is the job, and it is the build's **fourth** periodic policy — after the idle stop
//! compaction and the disk soft cap — and the first that *destroys* something on a
//! timer, so where it refuses is most of the work.
//!
//! Six rules it keeps:
//!
//! - **The candidates are the repository's rows, and the predicate is the story.** [`expired_before`] carries
//!   `status = 'stopped' AND stopped_at < cutoff`, so an `error` row is never auto-deleted (item 15's own
//!   sentence) and a `deleting` row is never a job's — that removal is the delete flow's in-flight work, and a
//!   second attempt at it would be two removals of one session.
//! - **It runs the delete flow, not a second removal.** [`complete_deletion`] is the same function the operator's
//!   `DELETE` goes through: the snapshot first, then the keep-rules (so a branch an agent committed to survives,
//!   ADR-0052/ADR-0065), then `deleting`, then the removal task. Nothing about keeping work is re-decided here,
//!   which is what "using the delete flow from item 9" means.
//! - **A snapshot that fails skips the row, and the skip is the retry.** The flow refuses with
//!   `409 unsaved_work`; a request would answer that and name `?force=true`, and a policy must not force — so it
//!   logs an `info` line and moves on, and the row is still a candidate tomorrow because nothing about it moved.
//! - **A row that left `stopped` is refused by the table.** The move into `deleting` is the transition table's
//!   narrow [`Trigger::Expire`] row (`stopped` only), so a resume that lands between this pass's read and its
//!   write is a refusal rather than a session removed under the operator (ADR-0016, ADR-0070).
//! - **The row moves before the container, and no lock spans the work.** The row is read in one short scope, the
//!   guard is dropped, and the snapshot and the backend's teardown are subprocess and daemon work outside it —
//!   the discipline the delete flow already keeps, and the reason a pass cannot stall every request.
//! - **It emits nothing of its own.** The flow already reports `session.status → deleting` (usually dropped,
//!   because the removal finishes inside the writer's tick), and item 9's "a delete is not a state change anyone
//!   else performs" holds. A row that is gone cannot carry a reason saying why.
//!
//! The tick is a constant rather than a config key, for the sweeper's reason (ADR-0025): the spec's config list is
//! exhaustive and the *day* is the policy — item 15 names the interval where it names `stopped_ttl_days` as a
//! key. **Its first pass is immediate**, and there the quota's `SETTLE` gate does not transfer: the startup
//! reconcile moves every *live* row to `starting`, and a `stopped` row is not live, so a pass taken the instant
//! this task spawns already sees every candidate. That is what makes the policy observable — backdate a row,
//! restart, read the row's disappearance — instead of a twenty-four-hour wait.

use std::time::Duration;

use tokio::task::JoinHandle;

use crate::clock;
use crate::http::AppState;
use crate::http::error::UNSAVED_WORK;
use crate::sessions::Session;
use crate::sessions::state::Trigger;

/// How often the job looks for sessions that have been stopped for too long (spec item 15's "a daily job").
///
/// A constant, not a config key: the interval *is* the policy, and the spec's config list does not carry it. It
/// bounds how late a removal can be, not when one happens — `stopped_ttl_days` is the number an operator sets.
pub const TICK: Duration = Duration::from_secs(24 * 60 * 60);

/// The moment before which a `stopped` row is past its window: `now - stopped_ttl_days`.
///
/// Pure, so a test pins it instead of reading a clock. Saturating twice over — [`clock::days`] for the
/// multiplication, and the subtraction here — because `stopped_ttl_days` is a `u64` the config layer accepts
/// without a ceiling: a wrapped window would turn "keep it for a week" into "remove it the moment it stops",
/// which is the one failure a destroying policy cannot have (the lesson `quota::over` and `turn::timeout`
/// carry). A window longer than the epoch is a cutoff before 1970, which matches every row — the honest answer
/// for "keep them for two hundred thousand years".
pub fn cutoff(now: i64, ttl_days: u64) -> i64 {
    let window = i64::try_from(clock::days(ttl_days).as_secs()).unwrap_or(i64::MAX);
    now.saturating_sub(window)
}

/// The stopped-session expiry's task: the state it asks the delete flow through, and the window it compares
/// against.
///
/// **It holds the [`AppState`] itself** rather than the four handles it uses, and that is the point: the whole
/// job is "call the delete flow's own second half", so a wiring of backend + transitioner + deleter +
/// `[merge] targets` here would be a second thing to keep right — exactly the drift this story exists to
/// prevent. The state is `Clone` and cheap (an `Arc` and two handles), and the policy owns no state of its own
/// beyond the window.
///
/// `Clone` for the same reason [`crate::sweeper::Sweeper`] is: `serve` builds one at startup and hands it to a
/// task.
#[derive(Debug, Clone)]
pub struct Expiry {
    state: AppState,
    ttl_days: u64,
}

impl Expiry {
    pub fn new(state: AppState, ttl_days: u64) -> Self {
        Self { state, ttl_days }
    }

    /// Hand the tick loop to a background task and return its handle.
    ///
    /// The handle is what stops it: `serve` aborts it once the listener's drain is over, so the job never
    /// outlives the process's own serving.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    /// Sweep, then sleep, until the task is aborted.
    ///
    /// The first sweep is immediate: see the module's note on why the quota's `SETTLE` gate does not apply.
    pub async fn run(&self) {
        loop {
            self.sweep().await;
            tokio::time::sleep(TICK).await;
        }
    }

    /// One pass: remove every session that has been stopped for longer than the window.
    ///
    /// Public so a test can await a pass instead of racing a spawned task; `spawn` is what production calls. A
    /// repository failure is logged and ends the pass — tomorrow is the retry, and a sweep that retried inside
    /// itself would spin on a closed database.
    pub async fn sweep(&self) {
        let now = clock::unix_now();
        let cutoff = cutoff(now, self.ttl_days);
        let candidates = match self.candidates(cutoff) {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::error!(
                    target: "holosmith::expiry",
                    %error,
                    "the stopped-session expiry's candidates could not be read"
                );
                return;
            }
        };
        tracing::debug!(
            target: "holosmith::expiry",
            candidates = candidates.len(),
            ttl_days = self.ttl_days,
            cutoff,
            "swept for sessions stopped past their retention window"
        );
        // Serial, and therefore bounded: a pass costs one snapshot plus one teardown per candidate, and nothing
        // anybody waits on is behind it — the tick is a day and the removal is the policy's answer, not a
        // request's. Awaiting it is also what lets a test assert the row the pass wrote instead of racing a
        // task.
        for session in candidates {
            self.expire(session).await;
        }
    }

    /// The candidates, oldest stop first: the repository's statement, so the predicate is one place.
    fn candidates(&self, cutoff: i64) -> Result<Vec<Session>, crate::sessions::RepoError> {
        let database = self
            .state
            .database
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        database.sessions().expired_before(cutoff)
    }

    /// Remove one candidate through the delete flow, or say why it was left alone.
    ///
    /// Every ending is one log line and nothing else: the row's own fate is the flow's, and a policy that
    /// reported through the event log would be inventing a vocabulary for a row that no longer exists.
    async fn expire(&self, session: Session) {
        // The row as it stands now, not as the pass read it: a resume or a delete that landed in between has
        // already answered, and the table would refuse the move anyway — this only keeps the line about it
        // honest.
        let current = match self.row(&session.id) {
            Ok(Some(current)) => current,
            // Gone: the delete flow finished it, or an operator did. Not a candidate any more.
            Ok(None) => {
                tracing::debug!(
                    target: "holosmith::expiry",
                    session = %session.id,
                    "the session was gone before the expiry reached it"
                );
                return;
            }
            Err(error) => {
                tracing::warn!(
                    target: "holosmith::expiry",
                    session = %session.id,
                    %error,
                    "the session could not be read before it was expired"
                );
                return;
            }
        };
        if current.status != crate::sessions::Status::Stopped {
            tracing::info!(
                target: "holosmith::expiry",
                session = %session.id,
                state = current.status.as_str(),
                "the session left `stopped` after the pass read it, so the expiry left it alone"
            );
            return;
        }
        // `force` is false and must stay false: the request's `?force=true` discards work the operator has been
        // told about, and a job has nobody to tell. The flow's refusal *is* the story's "skip and retry the next
        // day" — nothing about the row moves, so it is a candidate again tomorrow.
        match crate::http::complete_deletion(&self.state, current, Trigger::Expire, false, None)
            .await
        {
            Ok(_) => tracing::info!(
                target: "holosmith::expiry",
                session = %session.id,
                ttl_days = self.ttl_days,
                "a session stopped past its retention window was handed to the delete flow"
            ),
            Err(error) if error.body.code == UNSAVED_WORK => tracing::info!(
                target: "holosmith::expiry",
                session = %session.id,
                "the snapshot failed, so the session was not expired; it is a candidate again tomorrow"
            ),
            // Anything else is a machine failure rather than a decision — a state the table refused in the
            // window, a repository or a teardown that broke — so it is a `warn`: a `stopped` row this job can
            // never remove is something an operator has to see, and the sibling policy logs the equivalent
            // "cannot measure" the same way (ADR-0069).
            Err(error) => tracing::warn!(
                target: "holosmith::expiry",
                session = %session.id,
                status = error.status.as_u16(),
                code = error.body.code,
                message = %error.body.message,
                "the session was not expired"
            ),
        }
    }

    /// One row by id: the short scope [`Expiry::candidates`] uses, and nothing held across an await.
    fn row(&self, id: &str) -> Result<Option<Session>, crate::sessions::RepoError> {
        let database = self
            .state
            .database
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        database.sessions().get(id)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::backend::Backends;
    use crate::backend::{Call, StubBackend};
    use crate::config::Config;
    use crate::db::{Database, OpenOptions};
    use crate::events::{StreamRegistry, WriterOptions};
    use crate::merge::Merges;
    use crate::preflight::Tools;
    use crate::runtime::Backend;
    use crate::runtime::Runtime;
    use crate::sessions::{NewSession, Status};

    /// A database in a directory of this test's own, so no two tests share a file (ADR-0009's measured
    /// same-process "database is locked" trap).
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "holosmith-expiry-unit-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("a scratch directory");
            Self(path)
        }

        /// The database file inside it, which is the name  uses.
        fn database(&self) -> PathBuf {
            self.0.join("app.db")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A state over a fresh database, with the stub a test can still ask what it was told to do.
    ///
    /// Built here rather than reached for, because the thing under test is `Expiry::expire` — the pass's own
    /// decision about one row — and it is private for the reason `sweep` is public: production calls it one way,
    /// and the *interleaving* it guards against cannot be built from an integration test at all.
    fn state(dir: &Scratch) -> (AppState, StubBackend) {
        let database = Arc::new(Mutex::new(
            Database::open(&dir.database(), OpenOptions::default(), clock::unix_now())
                .expect("a fresh database opens"),
        ));
        let (events, _stream) =
            crate::events::spawn(Arc::clone(&database), WriterOptions::default());
        let stub = StubBackend::new(Duration::from_millis(1));
        (
            AppState {
                config: Config::default(),
                runtime: Runtime::started(Backend::Stub),
                database,
                events,
                streams: StreamRegistry::default(),
                backend: Backends::Stub(stub.clone()),
                drain: crate::drain::Control::new(),
                merges: Merges::new(),
                tools: Tools::default(),
                forwarding: crate::forward::Forwarding::default(),
            },
            stub,
        )
    }

    /// One row taken to `stopped` through the table, which is the only way a state changes in this crate
    /// (`no_status_write_outside_the_one_function` walks `src/**` and would refuse a raw write here — the right
    /// answer for a test that lives beside the policy).
    ///
    /// The row is handed to `expire` by hand and `complete_deletion` never consults the cutoff, so nothing here
    /// needs a backdated clock: the transition writes "now" and the test's subject is the *state* the row is in.
    async fn stopped_row(state: &AppState, key: &str) -> crate::sessions::Session {
        let created = {
            let database = state.database.lock().expect("the lock is not poisoned");
            database
                .sessions()
                .create(
                    &NewSession {
                        project: "holosmith".to_string(),
                        model: "claude-sonnet-4.5".to_string(),
                        thinking: "high".to_string(),
                        network: false,
                        client_key: Some(key.to_string()),
                        base_commit: None,
                    },
                    clock::unix_now(),
                )
                .expect("a session is created")
        };
        let transitioner = state.transitioner();
        transitioner
            .apply(&created.id, Trigger::PiReady, None)
            .await
            .expect("starting -> idle");
        transitioner
            .apply(&created.id, Trigger::IdleStop, Some("idle_stop"))
            .await
            .expect("idle -> stopped");
        state
            .database
            .lock()
            .expect("the lock is not poisoned")
            .sessions()
            .get(&created.id)
            .expect("the row reads back")
            .expect("the row is there")
    }

    #[tokio::test]
    async fn a_row_resumed_after_the_pass_read_it_is_left_alone_and_never_snapshotted() {
        // The interleaving the policy has to survive, made deterministic: the pass reads a candidate, the
        // operator resumes it, and the row the pass is holding is handed to `expire` anyway.
        //
        // What this isolates is the **status re-read**, and only that: deleting it makes `expire` hand a
        // `starting` row to the delete flow, the stub records a `Snapshot` of a session that is being
        // re-provisioned, and the first assertion goes red. (The move itself would still be refused — by
        // `Trigger::Expire`'s `stopped`-only row — so the second assertion is the backstop and not this test's
        // witness; `tests/state_machine.rs`'s `the_expirys_row_admits_stopped_and_nothing_else` is.)
        //
        // It is in the crate because `expire` is private: an integration test can only observe what a whole
        // `sweep` does, and by then a resumed row is not even a candidate — the statement's predicate answers
        // first, which is a different rule (`tests/expiry.rs` proves that one).
        let dir = Scratch::new("resumed");
        let (state, stub) = state(&dir);
        let stale = stopped_row(&state, "resumed").await;

        state
            .transitioner()
            .apply(&stale.id, Trigger::Resume, None)
            .await
            .expect("stopped -> starting");

        Expiry::new(state.clone(), Config::default().stopped_ttl_days)
            .expire(stale.clone())
            .await;

        let row = state
            .database
            .lock()
            .expect("the lock is not poisoned")
            .sessions()
            .get(&stale.id)
            .expect("the table is readable")
            .expect("a resumed session was removed by the expiry");
        assert_eq!(
            row.status,
            Status::Starting,
            "the expiry moved a row it did not own"
        );
        assert!(
            !stub.calls().contains(&Call::Snapshot),
            "the flow snapshotted a session it was not going to remove: {:?}",
            stub.calls()
        );
        assert!(
            !stub.calls().contains(&Call::Delete),
            "the expiry removed a resumed session: {:?}",
            stub.calls()
        );
    }

    #[test]
    fn the_tick_is_one_day_and_the_spec_says_so() {
        // The interval *is* the policy, so a number nothing can fail on is a number that will drift. Item 15's
        // word is "daily", and `case_stopped_expiry` cannot wait for one.
        assert_eq!(TICK, Duration::from_secs(86_400));
        assert_eq!(clock::days(1), TICK);
    }

    #[test]
    fn the_cutoff_is_the_window_subtracted_from_now() {
        let now = 1_700_000_000;
        assert_eq!(cutoff(now, 7), now - 7 * 86_400);
        assert_eq!(cutoff(now, 1), now - 86_400);
        // A zero window puts the cutoff at "now", and the comparison is strict: a row stopped exactly now is
        // not past it. (The config refuses `0`, so this is the arithmetic being total rather than a mode.)
        assert_eq!(cutoff(now, 0), now);
    }

    #[test]
    fn an_absurd_window_saturates_rather_than_wrapping() {
        // `stopped_ttl_days` is a `u64` the config layer accepts without a ceiling. A wrapped window would turn
        // "keep it for two hundred thousand years" into "remove it the moment it stops", which is the one
        // failure mode a destroying policy cannot have.
        assert_eq!(cutoff(1_700_000_000, u64::MAX), 1_700_000_000 - i64::MAX);
        assert!(cutoff(1_700_000_000, u64::MAX) < cutoff(1_700_000_000, 7));
        // And a window longer than the epoch is a cutoff before 1970, which matches every row: the honest
        // answer for "keep them for ever".
        assert!(cutoff(0, 7) < 0);
        assert_eq!(clock::days(u64::MAX).as_secs(), u64::MAX);
    }
}
