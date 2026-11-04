//! The idle stop's background half: a session nobody is using stops costing money (HS-021, ADR-0025).
//!
//! Spec item 13's second row is a policy, not an endpoint — `idle_stop_min` (default 30) — so
//! nothing a client does makes it happen. This module is the thing that happens: on a fixed tick it
//! reads every session that has been `idle` for longer than the key allows, and for each one applies
//! the table's `idle + idle_stop -> stopped` row and hands the backend's `stop` to a task of its own.
//!
//! Four rules it keeps:
//!
//! - **Only `idle` is a candidate, and that is the query.** `starting` and `running` are live and
//!   working; `error` and `deleting` are somebody else's to end (the delete flow's). The repository's
//!   `idle_before` carries the predicate, so there is no second place for the rule to be forgotten.
//! - **The row moves first, the container second, and the container's stop is best effort.** ADR-0020's
//!   order and ADR-0024's shape: the row is what frees the cap slot (item 13's own words), so the sweep
//!   does not wait for container work, and a stop that fails is a log line rather than a retry — the
//!   operator can delete a session whose container outlived it.
//! - **It is not a state machine.** Every state change goes through
//!   [`Transitioner::apply`](crate::sessions::state::Transitioner::apply). A refused application means
//!   the session left `idle` between the read and the move — an ordinary race with a prompt — which is
//!   an `info` line and not a bug.
//! - **It is one policy among siblings, not the only stop.** The cap evicts (HS-020), the drain stops
//!   everything (HS-044) and the turn timeout ends a turn (HS-022); all four use the table, and only the
//!   reason word differs ([`IDLE_STOP`] here).
//!
//! The tick is a constant rather than a config key, for the reason the stub's pacing is a constructor
//! argument (ADR-0015): the spec's config list is exhaustive, and how often a running process looks at a
//! column is not a machine setting. It bounds how late a stop can be, not when one happens.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::backend::{Backends, SessionBackend};
use crate::clock;
use crate::db::Database;
use crate::events::EventWriter;
use crate::sessions::Session;
use crate::sessions::state::{StateError, Transitioner, Trigger};

/// The reason an idle session's stop records (spec item 13).
///
/// The fourth reason word — `forced_abort`, `deleted` and `evicted` are the others — and the one a
/// client reads to tell "it stopped because I left it" from "it stopped to make room" and "I stopped it".
pub const IDLE_STOP: &str = "idle_stop";

/// How often the sweep looks, which bounds how late a stop can be by this much.
///
/// Five seconds: the policy is measured in minutes, so anything coarser would make `idle_stop_min = 1`
/// mean "somewhere between one and two minutes", and anything finer would be a wakeup the writer task
/// already makes sixty times a tick for no gain.
pub const TICK: Duration = Duration::from_secs(5);

/// The idle stop's handles and its two numbers.
///
/// `Clone` for the same reason [`crate::provisioner::Provisioner`] is: `serve` builds one at startup and
/// hands it to a task, and the fields are a backend value, an `Arc` and a sender.
#[derive(Debug, Clone)]
pub struct Sweeper {
    backend: Backends,
    database: Arc<Mutex<Database>>,
    events: EventWriter,
    idle_after: Duration,
}

impl Sweeper {
    pub fn new(
        backend: Backends,
        database: Arc<Mutex<Database>>,
        events: EventWriter,
        idle_after: Duration,
    ) -> Self {
        Self {
            backend,
            database,
            events,
            idle_after,
        }
    }

    /// Hand the tick loop to a background task and return its handle.
    ///
    /// The handle is what stops it: `serve` aborts it once the listener's drain is over, so the sweeper
    /// never outlives the process's own serving and no shutdown waits for a tick to come round.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    /// Sweep, then sleep, until the task is aborted.
    ///
    /// The first sweep is immediate: a process that restarted with quiet sessions should apply the
    /// policy now rather than a tick from now, and the tick after it is what paces the rest.
    pub async fn run(&self) {
        loop {
            self.sweep().await;
            tokio::time::sleep(TICK).await;
        }
    }

    /// One pass: stop every `idle` session that has been quiet for longer than `idle_after`.
    ///
    /// Public so a test can await a sweep instead of racing a spawned task; `spawn` is what production
    /// calls. A repository failure is logged and ends the pass: the next tick is the retry, and a sweep
    /// that retried inside itself would spin on a closed database.
    pub async fn sweep(&self) {
        let candidates = match self.candidates() {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::error!(
                    target: "holosmith::sweeper",
                    %error,
                    "the idle stop's candidates could not be read"
                );
                return;
            }
        };
        tracing::debug!(
            target: "holosmith::sweeper",
            idle_min = self.idle_minutes(),
            "swept for idle sessions"
        );
        for session in candidates {
            self.stop(session).await;
        }
    }

    /// Stop one candidate that the sweep has already chosen, and log an `info` line — not an error —
    /// when the table refuses.
    async fn stop(&self, session: Session) {
        match self
            .transitioner()
            .apply(&session.id, Trigger::IdleStop, Some(IDLE_STOP))
            .await
        {
            Ok(_) => {}
            // The row is `stopped` and only the report of it was lost: the process is on its way down,
            // and the container must still be stopped.
            Err(StateError::Unreported(_)) => {}
            Err(error) => {
                // `Illegal` is the ordinary race — the session was prompted, or deleted, between the
                // read and the move — and a repository failure is the database's. Neither is a bug, and
                // neither leaves anything behind: the row did not move, so no container is stopped.
                tracing::info!(
                    target: "holosmith::sweeper",
                    session = %session.id,
                    idle_min = self.idle_minutes(),
                    %error,
                    "the idle stop did not happen"
                );
                return;
            }
        }
        // The story's fourth bullet: one log line per stop. It is written here rather than beside the
        // spawn so that a stop the table refused has no line saying it happened.
        tracing::info!(
            target: "holosmith::sweeper",
            session = %session.id,
            idle_min = self.idle_minutes(),
            "the session sat idle for too long and was stopped"
        );
        // The row is `stopped`, which is what frees the slot, so the sweep must not wait for the
        // container: spec item 7's rule, and the eviction's precedent (ADR-0024). One wedged backend
        // then delays nothing but its own stop — not the rest of the sweep, and not the next tick.
        let stopping = self.backend.clone();
        let victim = session;
        tokio::spawn(async move {
            if let Err(error) = stopping.stop(&victim).await {
                tracing::warn!(
                    target: "holosmith::sweeper",
                    session = %victim.id,
                    %error,
                    "the backend did not stop; the row is still stopped"
                );
            }
        });
    }

    /// The sessions this pass may stop, quietest first.
    fn candidates(&self) -> Result<Vec<Session>, crate::sessions::RepoError> {
        let cutoff = clock::unix_now() - self.idle_after.as_secs() as i64;
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        database.sessions().idle_before(cutoff)
    }

    /// `idle_after` in whole minutes, for a log line: one spelling of the policy's number, so a line
    /// cannot report a duration no config key produced.
    fn idle_minutes(&self) -> u64 {
        self.idle_after.as_secs() / 60
    }

    /// The one state function, wired to this sweeper's two handles.
    fn transitioner(&self) -> Transitioner {
        Transitioner::new(Arc::clone(&self.database), self.events.clone())
    }
}
