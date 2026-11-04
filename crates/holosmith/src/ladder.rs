//! The abort ladder: the one routine that ends a turn that will not end itself (HS-016, ADR-0020).
//!
//! `POST /api/sessions/{id}/abort` answers 202 and this is what happens next, off the request's
//! task, so the answer never waits for a grace period (spec item 16). Two rungs, in the spec's
//! order:
//!
//! 1. [`SessionBackend::clear_queue`], then [`SessionBackend::abort`]: the agent is asked to stop,
//!    which is the cheap path and works whenever the agent is still listening.
//! 2. If the row is still `running` when `abort_grace` is spent, the container is killed: the row
//!    becomes `stopped` with the ladder's reason (`forced_abort` here), the backend is stopped, and
//!    the session is resumed at once, because a killed container is not a session the operator asked
//!    to keep stopped.
//!
//! **The grace covers rung 1 as well as the wait.** It runs from the abort, not from whenever the two
//! calls answer: an RPC that never returns is exactly the hang this routine exists for, so each call
//! is bounded by what is left of the grace rather than awaited forever.
//!
//! Four rules it keeps:
//!
//! - **It is shared, and its reason is a parameter.** HS-022's turn timeout, HS-023's budget and
//!   HS-019's delete climb the same ladder and pass their own reason, so the two-rung shape lives once
//!   ([`Ladder::climb`]'s `reason`). [`Ladder::end_turn`] is that shape without the way back, which is
//!   what a delete needs: it ends the turn and then removes the session, so resuming it would
//!   provision a container only to delete it.
//! - **The row moves before the container does, in that direction only.** Rung 2 applies
//!   `running -> stopped` *first*, then stops the backend: the other order leaves a row that says
//!   `idle` over a dead container, which is the one lie a client cannot detect (ADR-0016).
//! - **Rung 1 succeeding is the turn runner's ending, not this routine's.** A normal abort is the
//!   turn answering [`crate::backend::TurnEnd::Aborted`], which the runner writes as
//!   `running -> idle` with reason `abort_completed`. This routine only *waits* for it, and its
//!   own second rung is what makes the wait finite.
//! - **It is not a state machine.** Every state change goes through
//!   [`Transitioner::apply`](crate::sessions::state::Transitioner::apply); a refused application
//!   means the other owner ended the turn first, which is a race by construction and not a bug.
//!
//! The resume after rung 2 goes through the provisioner — `stopped -> starting` by `resume`, then
//! the four steps back to `idle` — which is the flow HS-018's `POST /resume` reuses. It does not
//! call [`SessionBackend::resume`]: whether a resumed session runs every provisioning step or only
//! the container's is HS-025's and HS-028's question, and answering it here would be a guess.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::backend::{BackendError, Backends, SessionBackend};
use crate::db::Database;
use crate::events::EventWriter;
use crate::provisioner::Provisioner;
use crate::sessions::RepoError;
use crate::sessions::Session;
use crate::sessions::Status;
use crate::sessions::state::{StateError, Transitioner, Trigger};

/// The reason rung 2 records when an abort had to kill the container (spec item 16).
pub const FORCED_ABORT: &str = "forced_abort";

/// How often the ladder looks at the row while it waits for rung 1.
///
/// A row read is one indexed statement through the mutex ADR-0012 introduced; 25 ms is the writer's
/// own commit tick (ADR-0013), so anything faster would ask for a state the event log cannot have
/// reported yet.
const POLL: Duration = Duration::from_millis(25);

/// One backend call, awaited no later than `deadline`.
///
/// A failure and a spent grace answer the same way — a reason a log line can print — because the
/// ladder treats both as "rung 1 did not work": the difference between an RPC that refused and one
/// that never answered is the agent's, and both are the hung agent rung 2 exists for.
async fn bounded(
    deadline: Instant,
    call: impl Future<Output = Result<(), BackendError>>,
) -> Result<(), String> {
    match tokio::time::timeout_at(deadline, call).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("did not answer before abort_grace".to_string()),
    }
}

/// How a running turn ended, as the ladder saw it.
///
/// What the caller does next is the only difference between the two: [`Self::Asked`] is rung 1
/// working, so the session is whatever the turn runner left it (`idle`); [`Self::Forced`] is rung 2,
/// so the row is `stopped` with the caller's reason and the backend has been told to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// The agent stopped: the turn runner wrote `running -> idle` (or the row had already moved).
    Asked,
    /// The grace was spent: the row was moved to `stopped` and the backend was stopped.
    Forced,
}

/// The abort ladder: two rungs, the row's state as its clock, and one reason per caller.
///
/// `Clone` for the same reason [`Provisioner`](crate::provisioner::Provisioner) and
/// [`crate::turn::Turn`] are: the abort handler builds one per accepted request and hands it to a
/// task.
#[derive(Debug, Clone)]
pub struct Ladder {
    backend: Backends,
    database: Arc<Mutex<Database>>,
    events: EventWriter,
    grace: Duration,
}

impl Ladder {
    pub fn new(
        backend: Backends,
        database: Arc<Mutex<Database>>,
        events: EventWriter,
        grace: Duration,
    ) -> Self {
        Self {
            backend,
            database,
            events,
            grace,
        }
    }

    /// Hand `session` to a background task and return immediately.
    ///
    /// `reason` is what a forced stop records; the abort's caller passes [`FORCED_ABORT`].
    pub fn spawn(self, session: Session, reason: &'static str) -> JoinHandle<()> {
        tokio::spawn(async move { self.climb(&session, reason).await })
    }

    /// Climb the ladder for `session`, which the caller has already checked is `running`.
    ///
    /// Public so a test can await it instead of racing a spawned task; `spawn` is what production
    /// calls. A forced stop is brought straight back: a killed container is not a session the
    /// operator asked to keep stopped.
    pub async fn climb(&self, session: &Session, reason: &'static str) {
        if self.end_turn(session, reason).await == Ending::Asked {
            return;
        }
        self.resume(session).await;
    }

    /// End the running turn for `session` and answer how, **without** bringing the session back.
    ///
    /// This is [`Self::climb`] minus the resume, and it exists for the delete flow (HS-019): a
    /// session about to be removed wants its turn over — the snapshot must not race the agent — and
    /// it emphatically does not want a fresh container provisioned on the way out.
    ///
    /// The caller has already checked that the session is `running`. The clock starts here, so the
    /// whole of it is bounded by `abort_grace` however a wedged backend behaves.
    pub async fn end_turn(&self, session: &Session, reason: &'static str) -> Ending {
        let deadline = Instant::now() + self.grace;
        self.rung_one(session, deadline).await;
        if self.the_turn_ended(session, deadline).await {
            return Ending::Asked;
        }
        if self.rung_two(session, reason).await {
            Ending::Forced
        } else {
            // Rung 2 refused: the turn ended between the last poll and here, which is rung 1
            // finishing late. The row is the runner's, so nothing was stopped.
            Ending::Asked
        }
    }

    /// Rung 1: drop what the agent has queued, then ask it to stop.
    ///
    /// Both failures are logged and neither ends the ladder: rung 2 exists precisely because the
    /// agent may not answer. A backend that cannot do either is a backend whose turn cannot stop,
    /// which is what the grace period and the forced rung are for. Each call is bounded by what is
    /// left of the grace, so an RPC that never returns is a spent grace rather than a hung ladder.
    async fn rung_one(&self, session: &Session, deadline: Instant) {
        if let Err(error) = bounded(deadline, self.backend.clear_queue(session)).await {
            tracing::warn!(
                target: "holosmith::ladder",
                session = %session.id,
                %error,
                "the agent's queue was not cleared"
            );
        }
        if let Err(error) = bounded(deadline, self.backend.abort(session)).await {
            tracing::warn!(
                target: "holosmith::ladder",
                session = %session.id,
                %error,
                "the agent was not asked to abort"
            );
        }
    }

    /// Wait until `deadline` for the turn to end, and say whether it did.
    ///
    /// The row is the question, not a flag: the turn runner writes `running -> idle` when the agent
    /// stops, so "the row is no longer `running`" is exactly "rung 1 worked". A row that is gone is
    /// also an ending — there is nothing left to force.
    async fn the_turn_ended(&self, session: &Session, deadline: Instant) -> bool {
        loop {
            match self.status(&session.id) {
                Ok(None) => return true,
                Ok(Some(status)) if status != Status::Running => return true,
                // Still running, or one unreadable row: keep waiting until the grace is spent.
                Ok(Some(_)) => {}
                Err(error) => tracing::warn!(
                    target: "holosmith::ladder",
                    session = %session.id,
                    %error,
                    "the ladder could not read the session"
                ),
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Rung 2: kill the container, record why, and answer whether it was this rung that stopped the
    /// session.
    ///
    /// The order is the decision (ADR-0020): the row becomes `stopped` before the backend is
    /// stopped, so no client ever sees `idle` over a dead container. A refused transition means the
    /// turn ended between the last poll and here, which is rung 1 finishing late — the session is
    /// then left exactly as the runner left it, and `false` says so.
    async fn rung_two(&self, session: &Session, reason: &'static str) -> bool {
        match self
            .transitioner()
            .apply(&session.id, Trigger::AbortLadder, Some(reason))
            .await
        {
            Ok(_) => {}
            // The state changed and only the report of it was lost, which means the process is on
            // its way down. The row is already `stopped`, so the container must still be killed.
            Err(StateError::Unreported(_)) => {}
            Err(StateError::Illegal { .. } | StateError::UnknownSession(_)) => {
                tracing::debug!(
                    target: "holosmith::ladder",
                    session = %session.id,
                    "the turn ended before the forced rung was needed"
                );
                return false;
            }
            Err(error) => {
                // The row could not be moved, so nothing may be killed: a container stopped behind
                // a row that still says `running` is the state the ladder exists to prevent.
                tracing::error!(
                    target: "holosmith::ladder",
                    session = %session.id,
                    %error,
                    "the forced rung could not stop the session"
                );
                return false;
            }
        }
        if let Err(error) = self.backend.stop(session).await {
            // Best effort, like the provisioner's undo: the row already says `stopped`, and the
            // operator can delete a session whose container outlived it.
            tracing::warn!(
                target: "holosmith::ladder",
                session = %session.id,
                %error,
                "the backend did not stop"
            );
        }
        true
    }

    /// Bring a session the ladder stopped back to `idle`, through the provisioner.
    ///
    /// `stopped -> starting` is the table's `resume` row and the four steps are the provisioner's,
    /// which is the same path HS-018's endpoint will take. A failure here leaves the row `stopped`
    /// with its reason, which is a state an operator can see and act on.
    async fn resume(&self, session: &Session) {
        match self
            .transitioner()
            .apply(&session.id, Trigger::Resume, None)
            .await
        {
            Ok(_) => {}
            // As above: the row is `starting` already, so it still needs a provisioner.
            Err(StateError::Unreported(_)) => {}
            Err(error) => {
                tracing::error!(
                    target: "holosmith::ladder",
                    session = %session.id,
                    %error,
                    "the forced rung could not resume the session"
                );
                return;
            }
        }
        match self.row(&session.id) {
            Ok(Some(resumed)) => {
                self.provisioner().spawn(resumed);
            }
            Ok(None) => tracing::warn!(
                target: "holosmith::ladder",
                session = %session.id,
                "the session vanished between the resume and its provisioning"
            ),
            Err(error) => tracing::error!(
                target: "holosmith::ladder",
                session = %session.id,
                %error,
                "the resumed session could not be read back"
            ),
        }
    }

    /// The row's current state, or `None` when there is no such row.
    fn status(&self, session_id: &str) -> Result<Option<Status>, RepoError> {
        Ok(self.row(session_id)?.map(|session| session.status))
    }

    /// The row itself: the provisioner is handed the row as it stands after the resume.
    fn row(&self, session_id: &str) -> Result<Option<Session>, RepoError> {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        database.sessions().get(session_id)
    }

    /// A provisioner wired to this ladder's handles, for the resume step.
    fn provisioner(&self) -> Provisioner {
        Provisioner::new(
            self.backend.clone(),
            Arc::clone(&self.database),
            self.events.clone(),
        )
    }

    /// The one state function, wired to this ladder's two handles.
    fn transitioner(&self) -> Transitioner {
        Transitioner::new(Arc::clone(&self.database), self.events.clone())
    }
}
