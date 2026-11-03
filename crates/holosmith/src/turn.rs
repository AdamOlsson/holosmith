//! The turn runner: the background work one prompt becomes (HS-015).
//!
//! `POST /api/sessions/{id}/prompt` moves the row to `running` and answers 202; this is what
//! happens next, off the request's task, so the answer never waits for a turn (spec item 8). It
//! touches `last_active_at`, asks the backend to run the turn — the backend emits the turn's frames,
//! the server decides every state (D6) — and then ends the turn: `agent_settled` takes the session
//! back to `idle` on a normal finish, and `unexpected_exit` takes it to `error` with the backend's
//! reason on a failed one (spec item 6: "any other exit means error").
//!
//! Three rules it keeps, the same three the provisioner keeps:
//!
//! - **The prompt's own transition is the handler's**, not this task's: the row has to say `running`
//!   before the 202 is written, so the handler applies `prompt`, refetches the row and answers it, and
//!   only then spawns this. A runner that applied `prompt` itself would either move the row a second
//!   time (impossible: the table has no `running + prompt` row) or race the answer it is behind.
//! - **No lock is held across an `.await`.** A touch and every state change are their own
//!   synchronous statements through `Arc<Mutex<Database>>` (ADR-0012/ADR-0013), so the guard cannot
//!   outlive a statement and the event writer's commit never waits on this task.
//! - **This is not a state machine.** HS-012 owns the table and the one function that writes a
//!   state; the runner asks it for `agent_settled` or `unexpected_exit` and writes nothing itself.
//!
//! Abort and steer are HS-016's and HS-017's: the script is cut short by the backend's own
//! `abort`/`stop`, and the ladder that asks for it is [`crate::ladder`].

use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;

use crate::backend::{Backends, SessionBackend, TurnEnd};
use crate::db::Database;
use crate::events::EventWriter;
use crate::sessions::Session;
use crate::sessions::activity;
use crate::sessions::state::{StateError, Transitioner, Trigger};

/// The reason a turn that ended because of an abort records on `running -> idle`.
///
/// The event's payload has no trigger field (ADR-0016), so the reason is the only place a client
/// can read *how* a turn ended: `abort_completed` here, [`crate::ladder::FORCED_ABORT`] when the
/// ladder had to kill the container. Spelling the trigger's own word keeps one vocabulary.
pub const ABORT_COMPLETED: &str = "abort_completed";

/// The turn runner's handles: where a turn runs, where the row is, and where its frames go.
///
/// `Clone` for the same reason [`crate::provisioner::Provisioner`] is: the prompt handler builds one
/// per accepted prompt and hands it to a task, and the three fields are a backend value, one `Arc`
/// and a sender, so cloning copies no state.
#[derive(Debug, Clone)]
pub struct Turn {
    backend: Backends,
    database: Arc<Mutex<Database>>,
    events: EventWriter,
}

impl Turn {
    pub fn new(backend: Backends, database: Arc<Mutex<Database>>, events: EventWriter) -> Self {
        Self {
            backend,
            database,
            events,
        }
    }

    /// Hand `session` and `prompt` to a background task and return immediately.
    ///
    /// The task owns everything it needs, so the caller drops its references the moment the answer
    /// is built: no response ever waits for a turn.
    pub fn spawn(self, session: Session, prompt: String) -> JoinHandle<()> {
        tokio::spawn(async move { self.run(session, prompt).await })
    }

    /// Run one turn for `session`, which the caller has already moved to `running`.
    ///
    /// Public so a test can await it instead of racing a spawned task; `spawn` is what production
    /// calls.
    pub async fn run(self, session: Session, prompt: String) {
        self.touch(&session.id);
        match self
            .backend
            .start_turn(&session, &prompt, &self.events)
            .await
        {
            Ok(TurnEnd::Settled) => {
                self.transition(&session, Trigger::AgentSettled, None).await;
                self.touch(&session.id);
            }
            Ok(TurnEnd::Aborted) => {
                self.abort_ended(&session).await;
                self.touch(&session.id);
            }
            Err(error) => {
                // The error's own text is the reason the row and the event carry: it is written for
                // the operator (a backend's refusal names the operation it could not do).
                let reason = error.to_string();
                tracing::warn!(
                    target: "holosmith::turn",
                    session = %session.id,
                    %error,
                    "a turn ended with an error"
                );
                self.transition(&session, Trigger::UnexpectedExit, Some(&reason))
                    .await;
            }
        }
    }

    /// The abort ending: `running -> idle` on `abort_completed`, unless the ladder got there first.
    ///
    /// Two tasks can end one turn (HS-016, ADR-0020): the ladder's second rung exists exactly for a
    /// turn that did not stop, so it applies `running -> stopped` and then stops the backend, which
    /// is what finally makes this turn answer [`TurnEnd::Aborted`]. The row is therefore often no
    /// longer `running` by the time this runs, and the table refusing the row is the ordinary
    /// outcome of the two owners rather than a bug — so it is a `debug` line here, and only here.
    async fn abort_ended(&self, session: &Session) {
        match self
            .transitioner()
            .apply(&session.id, Trigger::AbortCompleted, Some(ABORT_COMPLETED))
            .await
        {
            Ok(_) => {}
            Err(error @ StateError::Illegal { .. }) => tracing::debug!(
                target: "holosmith::turn",
                session = %session.id,
                %error,
                "the abort ladder ended this turn instead"
            ),
            Err(error) => tracing::warn!(
                target: "holosmith::turn",
                session = %session.id,
                %error,
                "a turn's abort ending did not complete"
            ),
        }
    }

    /// Record that the session was active: `last_active_at` moves at the prompt and at the settle,
    /// which is what HS-020's LRU order and HS-021's idle stop read.
    ///
    /// The rule that a failed touch is logged rather than fatal lives in
    /// [`crate::sessions::activity`], because the steer handler moves the same column for the same
    /// reason.
    fn touch(&self, session_id: &str) {
        activity::touch(&self.database, session_id);
    }

    /// One state change, with a failure logged rather than fatal: the row is the turn's outcome and
    /// the event is a report of it, so a refused transition (a bug) or a stopped writer must not make
    /// the task lie about what happened.
    async fn transition(&self, session: &Session, trigger: Trigger, reason: Option<&str>) {
        // The reporting is the state module's, so this runner and the provisioner cannot drift
        // apart in how they log a refusal.
        self.transitioner()
            .apply_logged(&session.id, trigger, reason)
            .await;
    }

    /// The one state function, wired to this runner's two handles.
    fn transitioner(&self) -> Transitioner {
        Transitioner::new(Arc::clone(&self.database), self.events.clone())
    }
}
