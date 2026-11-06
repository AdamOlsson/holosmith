//! Compaction: an old message's deltas become one snapshot (HS-060, spec item 15).
//!
//! A streaming agent writes far more `message.delta` rows than a transcript needs, and HS-059's
//! coalescing only bounds them per commit. This module is the other half of the spec's answer: once a
//! message is a day old, nobody will watch it arrive, so its deltas are folded into one
//! `message.snapshot` — the whole text and thinking, applied by message id, replacing rather than
//! appending (ADR-0018 already pinned the frame and the renderer already replaces on it).
//!
//! Four rules it keeps:
//!
//! - **Only a completed message is a candidate.** `message.end` is the only durable instant that says a
//!   message finished, and it is what the window measures. A message still being streamed has no
//!   `message.end`, so a turn in flight cannot have its text folded away underneath it.
//! - **It never runs while a turn is running for that session, and the row is re-read inside the lock.**
//!   The whole per-session pass runs while this task holds the database mutex, and
//!   [`Transitioner::apply`](crate::sessions::state::Transitioner::apply) takes the same mutex for every
//!   state change. So a turn either started before the pass — and the row says `running`, which the pass
//!   skips — or it starts after the pass is over. There is no window between the read and the write for a
//!   prompt to slip into, and no second lock to keep in step (ADR-0032).
//! - **The snapshot takes the highest seq of the rows it removed, and the counter is not touched.**
//!   `sessions.last_seq` is what must never rewind; a snapshot reuses a seq the log no longer holds, which
//!   is why spec item 15 allows gaps exactly here (ADR-0013).
//! - **Nothing is broadcast.** A client already connected has the deltas and its transcript is unchanged;
//!   a client that reconnects reads the snapshot from its replay. Only the writer assigns a seq and
//!   broadcasts after its own commit, and compaction is not the writer.
//!
//! The window is a **constant**, not a config key, for the reason the sweeper's tick is (ADR-0025) and the
//! stub's pacing is (ADR-0015): spec item 15 names 24 hours and the spec's config list is exhaustive, so a
//! key here would be an invented setting. The constructor takes it anyway, so a test chooses its own.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::clock;
use crate::db::Database;
use crate::events::{Compaction, RepoError};
use crate::sessions::Status;

/// How old a message must be before its deltas become a snapshot (spec item 15).
pub const SNAPSHOT_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// How often a long-lived process looks for messages that have crossed the window.
///
/// A minute: the policy is measured in days, so this only bounds how late a snapshot is, and one
/// candidates read a minute is nothing next to the writer's own sixty ticks in the same span. A restart
/// does not wait for it — [`Compactor::run`] sweeps before its first sleep.
pub const TICK: Duration = Duration::from_secs(60);

/// The most messages one session is folded by in one pass.
///
/// A bound rather than "all of them" because the pass holds the database mutex, which the writer and every
/// handler need: a session that has been neglected for a month should cost several bounded transactions,
/// not one unbounded one. The next tick continues where this pass stopped.
pub const MESSAGES_PER_PASS: u32 = 500;

/// The compaction task's handles and its one number.
///
/// `Clone` for the reason [`crate::sweeper::Sweeper`] is: `serve` builds one at startup and hands it to a
/// task, and the fields are an `Arc` and a `Duration`.
#[derive(Debug, Clone)]
pub struct Compactor {
    database: Arc<Mutex<Database>>,
    after: Duration,
}

impl Compactor {
    /// A compactor over `database`, folding messages older than [`SNAPSHOT_AFTER`].
    pub fn new(database: Arc<Mutex<Database>>) -> Self {
        Self::with_window(database, SNAPSHOT_AFTER)
    }

    /// [`Self::new`] with the window handed in.
    ///
    /// Production has one caller and it passes the constant; a test passes a window it can produce rows
    /// for. The window is a `Duration` rather than a config key, so this is the only way to shorten it —
    /// which is the point: there is no second spelling of "how old is old" for an operator to disagree
    /// with the spec about (ADR-0032).
    pub fn with_window(database: Arc<Mutex<Database>>, after: Duration) -> Self {
        Self { database, after }
    }

    /// Hand the tick loop to a background task and return its handle.
    ///
    /// The handle is what stops it: `serve` aborts it once the listener's drain is over, so compaction
    /// never outlives the process's own serving and no shutdown waits for a tick to come round.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    /// Sweep, then sleep, until the task is aborted.
    ///
    /// The first sweep is immediate, and that is load-bearing rather than tidy: a process that restarted
    /// with a day-old log folds it now instead of a minute from now, which is also what makes the gate's
    /// own case affordable (`case_compaction` restarts the artifact and reads the replay).
    pub async fn run(&self) {
        loop {
            self.sweep().await;
            tokio::time::sleep(TICK).await;
        }
    }

    /// One pass: fold every eligible message of every session that is not live.
    ///
    /// Public so a test can await a pass instead of racing a spawned task; `spawn` is what production
    /// calls. A repository failure is logged and ends the pass: the next tick is the retry, and a pass that
    /// retried inside itself would spin on a closed database.
    pub async fn sweep(&self) {
        let candidates = match self.candidates() {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::error!(
                    target: "holosmith::compactor",
                    %error,
                    "the sessions compaction may fold could not be read"
                );
                return;
            }
        };
        for id in candidates {
            match self.fold(&id) {
                Ok(compaction) if compaction.messages > 0 => {
                    tracing::info!(
                        target: "holosmith::compactor",
                        session = %id,
                        messages = compaction.messages,
                        rows = compaction.rows,
                        "old messages were compacted into snapshots"
                    );
                }
                Ok(compaction) if compaction.unreadable > 0 => {
                    // The session-wide guard wrote nothing, so nothing is lost — but the policy has
                    // stopped for this session until the producer is fixed, and silence would make
                    // that look like "there was nothing to fold". One line per pass is the point.
                    tracing::warn!(
                        target: "holosmith::compactor",
                        session = %id,
                        unreadable = compaction.unreadable,
                        "a delta the contract does not describe stopped this session's compaction"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::error!(
                        target: "holosmith::compactor",
                        session = %id,
                        %error,
                        "a session could not be compacted"
                    );
                }
            }
            // One session's pass can be hundreds of messages of parsing and writing; yielding between
            // sessions keeps the runtime able to run the writer and a handler while a sweep is going on.
            tokio::task::yield_now().await;
        }
    }

    /// The sessions this pass may fold: every one whose state says no turn is running and no removal is
    /// in flight.
    fn candidates(&self) -> Result<Vec<String>, RepoError> {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        let sessions = database
            .sessions()
            .list(None)
            .map_err(RepoError::Sessions)?;
        Ok(sessions
            .into_iter()
            .filter(|session| foldable(session.status))
            .map(|session| session.id)
            .collect())
    }

    /// Fold one session's eligible messages, inside the lock and with the state re-read in it.
    ///
    /// The re-read is the story's fourth acceptance line and the whole reason the lock is held here: a
    /// prompt that moved the row to `running` between [`Self::candidates`] and now makes this a no-op, and
    /// a prompt cannot move it while this is running.
    ///
    /// Public because it is where that rule lives, and a test has to be able to ask it directly: a pass
    /// filters its candidates *before* it takes the lock, so a test that only ever calls [`Self::sweep`]
    /// cannot tell the filter from the authority.
    pub fn fold(&self, id: &str) -> Result<Compaction, RepoError> {
        let cutoff = cutoff(self.after);
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(session) = database.sessions().get(id).map_err(RepoError::Sessions)? else {
            // Removed between the read and now: nothing to fold, and not a failure.
            return Ok(Compaction::default());
        };
        if !foldable(session.status) {
            tracing::debug!(
                target: "holosmith::compactor",
                session = %id,
                status = session.status.as_str(),
                "a session was skipped because its state is not one compaction may fold"
            );
            return Ok(Compaction::default());
        }
        database.events().compact(id, cutoff, MESSAGES_PER_PASS)
    }
}

/// The states in which no turn is running for the session and no removal is in flight.
///
/// `running` is the state the story forbids, and `deleting` is a row on its way out whose log is about to go
/// with it. What is left is every state in which the log is final for now — **including `starting`**, since
/// HS-044: a row being provisioned has no turn running (a prompt needs `idle`) and no removal in flight, and
/// the startup reconcile makes every live row `starting` for as long as its re-provision takes. Excluding it
/// would mean a restarted machine with a day-old log folded nothing until the next tick — the gate's own
/// `case_compaction` caught exactly that. The safety argument is ADR-0032's either way: the pass holds the
/// database mutex the transitioner takes, so a session either was `starting` when the pass read it or became
/// so after the pass committed.
fn foldable(status: Status) -> bool {
    matches!(
        status,
        Status::Starting | Status::Idle | Status::Stopped | Status::Error
    )
}

/// The oldest `created_at` a message may carry and still be folded.
///
/// Saturating, not wrapping: the window is a `Duration` the constructor accepts without a ceiling, and a
/// wrapped cutoff would fold every message on the machine — the opposite of what "more than 24 hours old"
/// means. The same argument [`crate::clock::minutes`] makes for a config key.
fn cutoff(after: Duration) -> i64 {
    let seconds = i64::try_from(after.as_secs()).unwrap_or(i64::MAX);
    clock::unix_now().saturating_sub(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_longer_than_the_clock_cannot_fold_everything() {
        // A wrapped cutoff would be a *later* instant than now, and `created_at < cutoff` would then hold
        // for every row on the machine. Saturating makes an absurd window fold nothing instead, and a
        // window of zero folds only what is already in the past.
        let absurd = cutoff(Duration::MAX);
        assert!(
            absurd < clock::unix_now() - 1_000_000_000,
            "a window longer than the clock produced a cutoff that would fold everything: {absurd}"
        );
        assert!(
            cutoff(Duration::ZERO) > cutoff(Duration::from_secs(60)),
            "a longer window must reach further back"
        );
    }

    #[test]
    fn the_window_is_the_specs_day() {
        // Spec item 15's number, in the one place it is spelt. It is not a config key (ADR-0032), so this
        // constant is the whole definition of "old".
        assert_eq!(SNAPSHOT_AFTER, Duration::from_secs(24 * 60 * 60));
    }

    #[test]
    fn a_turn_in_flight_is_never_folded() {
        // The states the story names, `starting` (HS-044: no turn runs there, and a restart parks every live
        // row in it), and `deleting`, which is a log on its way out.
        assert!(foldable(Status::Starting));
        assert!(foldable(Status::Idle));
        assert!(foldable(Status::Stopped));
        assert!(foldable(Status::Error));
        assert!(!foldable(Status::Running));
        assert!(!foldable(Status::Deleting));
    }
}
