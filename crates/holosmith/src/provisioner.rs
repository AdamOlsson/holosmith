//! The provisioner: the background work that turns an accepted session into a running one,
//! reporting progress by step (HS-011).
//!
//! `POST /api/sessions` writes the row and answers 202; this is what happens next, off the
//! request's task, so a slow start never looks like a failed tap (spec item 7). It runs the four
//! steps in order, emits one `session.status` event as each begins, and ends the session in
//! `idle` — or, on a failure, undoes the steps that completed and ends it in `error` with a
//! reason.
//!
//! Three rules it keeps, each with a story behind it:
//!
//! - **The row moves first, the event follows.** An event is a claim about durable state, so the
//!   write happens before the announcement; a client that acts on `idle` finds a row that says
//!   `idle`. The step events claim nothing about the row, so they are emitted before the step
//!   runs — that is what makes a slow start legible *while* it is slow.
//! - **No lock is held across an `.await`.** Every status write is one synchronous block through
//!   `Arc<Mutex<Database>>` (ADR-0012/ADR-0013); the guard is not `Send`, so the compiler
//!   enforces it, and the event writer's commit can never wait on this task.
//! - **This is not a state machine.** HS-012 owns the table and the one function that writes a
//!   state. The provisioner asks it for `starting → idle` (`PiReady`) or `starting → error`
//!   (`ProvisioningFailed`) and emits the four step reports itself, because a step beginning is
//!   not a state change: its payload says `from: starting, to: starting, step: …`.

use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;

use crate::backend::{Backends, SessionBackend, Step};
use crate::clock;
use crate::db::Database;
use crate::events::{EventWriter, NewEvent};
use crate::sessions::state::{SESSION_STATUS, SessionStatus, Transitioner, Trigger};
use crate::sessions::{Session, Status};

/// The provisioner's handles: where to run steps, where the row is, and where events go.
///
/// `Clone` because the create handler builds one per accepted session and hands it to a task; the
/// three fields are a backend value, one `Arc` and a sender, so cloning copies no state.
#[derive(Debug, Clone)]
pub struct Provisioner {
    backend: Backends,
    database: Arc<Mutex<Database>>,
    events: EventWriter,
}

impl Provisioner {
    pub fn new(backend: Backends, database: Arc<Mutex<Database>>, events: EventWriter) -> Self {
        Self {
            backend,
            database,
            events,
        }
    }

    /// Hand `session` to a background task and return immediately.
    ///
    /// The task owns everything it needs, so the caller drops its references the moment the
    /// response is built: the answer never waits for a step.
    pub fn spawn(self, session: Session) -> JoinHandle<()> {
        tokio::spawn(async move { self.run(session).await })
    }

    /// Provision `session`: the four steps in order, then `idle`, or the undo and `error`.
    ///
    /// Public so a test can await it instead of racing a spawned task; `spawn` is what
    /// production calls.
    pub async fn run(&self, session: Session) {
        let mut completed: Vec<Step> = Vec::new();
        for step in Step::ALL {
            self.announce(&session, step).await;
            match self.backend.provision(&session, step).await {
                Ok(()) => completed.push(step),
                Err(error) => {
                    let reason = error.to_string();
                    tracing::warn!(
                        target: "holosmith::provisioner",
                        session = %session.id,
                        step = step.as_str(),
                        %error,
                        "a provisioning step failed"
                    );
                    self.undo(&session, &completed).await;
                    self.fail(&session, reason).await;
                    return;
                }
            }
        }
        self.settle(&session).await;
    }

    /// Undo the steps that completed, most recent first.
    ///
    /// Best effort on purpose: a step that cannot be undone is logged and the rest still run,
    /// because leaving two partial resources behind is worse than leaving one. The stub backend
    /// records these calls even though it created nothing — the Docker backend's teardown is what
    /// fills the shape, and it must not be invented there first.
    async fn undo(&self, session: &Session, completed: &[Step]) {
        for step in completed.iter().rev() {
            if let Err(error) = self.backend.undo(session, *step).await {
                tracing::warn!(
                    target: "holosmith::provisioner",
                    session = %session.id,
                    step = step.as_str(),
                    %error,
                    "a provisioning step could not be undone"
                );
            }
        }
    }

    /// The success ending: `starting -> idle` through the one state function, which writes the row
    /// and emits the event in that order.
    async fn settle(&self, session: &Session) {
        self.transition(session, Trigger::PiReady, None).await;
    }

    /// The failure ending: `starting -> error` with `reason`, through the same function.
    async fn fail(&self, session: &Session, reason: String) {
        self.transition(session, Trigger::ProvisioningFailed, Some(&reason))
            .await;
    }

    /// One state change, with a failure logged rather than fatal: the row is the provisioner's
    /// outcome and the event is a report of it, so a refused transition (a bug) or a stopped
    /// writer must not make the task lie about what happened.
    async fn transition(&self, session: &Session, trigger: Trigger, reason: Option<&str>) {
        // The reporting is the state module's, so this runner and the turn cannot drift apart in
        // how they log a refusal.
        self.transitioner()
            .apply_logged(&session.id, trigger, reason)
            .await;
    }

    /// The one state function, wired to this provisioner's two handles.
    fn transitioner(&self) -> Transitioner {
        Transitioner::new(Arc::clone(&self.database), self.events.clone())
    }

    /// Append one step report: `session.status` with the step that is now running.
    ///
    /// A step beginning changes no state, so `from` and `to` are both `starting`; the transition
    /// itself is the state machine's, and nothing here writes the row.
    ///
    /// A failed append means the writer has stopped, which in a served process means the process
    /// is going down; the loss is logged and the provisioner carries on rather than failing a
    /// session over a dying process.
    async fn announce(&self, session: &Session, step: Step) {
        // `starting` is the state provisioning runs in: the create writes it and only this task
        // moves the row from it, so a step report can name it without reading the row back. A
        // later story that provisions after a resume writes `starting` too (spec item 8's resume
        // row), which is why this stays true rather than becoming a guess.
        let payload = SessionStatus::step(Status::Starting, step);
        let event = NewEvent {
            session_id: session.id.clone(),
            kind: SESSION_STATUS.to_string(),
            payload: payload.to_json(),
            created_at: clock::unix_now(),
        };
        if let Err(error) = self.events.append(event).await {
            tracing::error!(
                target: "holosmith::provisioner",
                session = %session.id,
                %error,
                "a session.status event was not queued"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provisioner_reports_the_state_machines_event_type() {
        assert_eq!(SESSION_STATUS, "session.status");
    }

    #[test]
    fn a_step_report_names_the_step_and_claims_no_state_change() {
        // The four progress events are the state machine's payload with `step` set and no
        // transition: `from` and `to` are the state the session is in.
        assert_eq!(
            SessionStatus::step(Status::Starting, Step::Worktree).to_json(),
            r#"{"from":"starting","to":"starting","step":"worktree","reason":null}"#
        );
    }
}
