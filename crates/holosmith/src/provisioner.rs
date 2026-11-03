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
//! - **This is not a state machine.** It writes exactly two transitions, `starting → idle` and
//!   `starting → error`, through the repository's one status writer. HS-012 owns the table, the
//!   illegal-transition refusal and the event's `from`/`to`.

use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tokio::task::JoinHandle;

use crate::backend::{Backends, SessionBackend, Step};
use crate::clock;
use crate::db::Database;
use crate::events::{EventWriter, NewEvent};
use crate::sessions::{Session, Status};

/// The event type every provisioning report carries.
pub const SESSION_STATUS: &str = "session.status";

/// The payload of a `session.status` event: the step that is running, the state the session is
/// in, and the reason when there is one.
///
/// `Option` fields are explicit `null` and the three keys are always present, the same rule the
/// session row follows: one stable shape, so a client decodes it once. `step` is `None` on the
/// event that reports the end of provisioning, because no step is running then.
///
/// HS-012 replaces `status` with `from` and `to`: the state this story reports is the one the row
/// holds, and the transition table is what will say which moves are legal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusPayload {
    pub step: Option<Step>,
    pub status: Status,
    pub reason: Option<String>,
}

impl StatusPayload {
    fn new(step: Option<Step>, status: Status, reason: Option<String>) -> Self {
        Self {
            step,
            status,
            reason,
        }
    }

    /// The payload as the `events.payload` column holds it.
    fn to_json(&self) -> String {
        serde_json::to_string(self).expect("three string-ish fields always serialise")
    }
}

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
            self.announce(&session, Some(step), Status::Starting, None)
                .await;
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
                    self.fail(&session, step, reason).await;
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

    /// The success ending: the row is `idle` before the event says so.
    async fn settle(&self, session: &Session) {
        if !self.set_status(&session.id, Status::Idle, None) {
            return;
        }
        self.announce(session, None, Status::Idle, None).await;
    }

    /// The failure ending: the row is `error` with `reason` before the event carries it.
    async fn fail(&self, session: &Session, step: Step, reason: String) {
        if !self.set_status(&session.id, Status::Error, Some(&reason)) {
            return;
        }
        self.announce(session, Some(step), Status::Error, Some(reason))
            .await;
    }

    /// One status write, with the database's lock held for the statement and nothing else.
    ///
    /// `false` means the row is gone, which during provisioning only happens when the session was
    /// deleted underneath it: the honest outcome then is to stop, not to resurrect it. HS-019
    /// owns the delete flow that makes this reachable on purpose.
    fn set_status(&self, session_id: &str, status: Status, reason: Option<&str>) -> bool {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        match database
            .sessions()
            .set_status(session_id, status, reason, clock::unix_now())
        {
            Ok(Some(_)) => true,
            Ok(None) => {
                tracing::info!(
                    target: "holosmith::provisioner",
                    session = session_id,
                    %status,
                    "the session vanished while it was provisioning, so provisioning stopped"
                );
                false
            }
            Err(error) => {
                tracing::error!(
                    target: "holosmith::provisioner",
                    session = session_id,
                    %error,
                    "a provisioning status write failed"
                );
                false
            }
        }
    }

    /// Append one `session.status` event.
    ///
    /// A failed append means the writer has stopped, which in a served process means the process
    /// is going down; the row is already correct, so the loss of the event is logged and the
    /// provisioner carries on rather than failing a session over a dying process.
    async fn announce(
        &self,
        session: &Session,
        step: Option<Step>,
        status: Status,
        reason: Option<String>,
    ) {
        let payload = StatusPayload::new(step, status, reason);
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
    fn the_payload_is_three_keys_in_order_with_explicit_nulls() {
        let running = StatusPayload::new(Some(Step::Network), Status::Starting, None);
        assert_eq!(
            running.to_json(),
            r#"{"step":"network","status":"starting","reason":null}"#
        );
        let settled = StatusPayload::new(None, Status::Idle, None);
        assert_eq!(
            settled.to_json(),
            r#"{"step":null,"status":"idle","reason":null}"#
        );
        let failed = StatusPayload::new(
            Some(Step::Container),
            Status::Error,
            Some("the image is missing".to_string()),
        );
        assert_eq!(
            failed.to_json(),
            r#"{"step":"container","status":"error","reason":"the image is missing"}"#
        );
    }

    #[test]
    fn the_event_type_is_the_one_the_ui_and_the_story_name() {
        assert_eq!(SESSION_STATUS, "session.status");
    }
}
