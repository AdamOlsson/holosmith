//! The stopped-session expiry: a session nobody came back to is removed, so the operator does not prune by
//! hand (HS-064, spec item 15, decision D3).
//!
//! Item 15's retention bullet is a policy, not an endpoint: "a daily job deletes sessions that have been stopped
//! for more than 7 days (config `stopped_ttl_days`, default 7), using the delete flow from item 9. A snapshot
//! runs first; if it fails, the session is skipped and retried the next day. Sessions in error are never
//! auto-deleted." This module is the job, and it is the build's **fourth** periodic policy — after the idle stop
//! (HS-021), compaction (HS-060) and the disk soft cap (HS-063) — and the first that *destroys* something on a
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
            Err(error) => tracing::info!(
                target: "holosmith::expiry",
                session = %session.id,
                status = error.status.as_u16(),
                code = error.body.code,
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
    use super::*;

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
        // Nothing is past a zero window, and a row stopped exactly "now" is not older than "now".
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
