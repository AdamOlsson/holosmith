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
//! Abort and steer are HS-016's and HS-017's: the script is unconditional today, and a stop signal a
//! backend could raise arrives with the story that needs it.

use std::sync::{Arc, Mutex, PoisonError};

use tokio::task::JoinHandle;

use crate::backend::{Backends, SessionBackend};
use crate::clock;
use crate::db::Database;
use crate::events::EventWriter;
use crate::sessions::Session;
use crate::sessions::state::{Transitioner, Trigger};

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
            Ok(()) => {
                self.transition(&session, Trigger::AgentSettled, None).await;
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

    /// Record that the session was active: `last_active_at` moves at the prompt and at the settle,
    /// which is what HS-020's LRU order and HS-021's idle stop read.
    ///
    /// A failed touch is logged and does not end the turn: the row's activity is a hint for
    /// eviction, and losing it is not a reason to strand a session in `running`.
    fn touch(&self, session_id: &str) {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(error) = database.sessions().touch(session_id, clock::unix_now()) {
            tracing::warn!(
                target: "holosmith::turn",
                session = session_id,
                %error,
                "last_active_at did not move"
            );
        }
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
