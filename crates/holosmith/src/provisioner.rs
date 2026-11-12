//! The provisioner: the background work that turns an accepted session into a running one,
//! reporting progress by step.
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
//! - **This is not a state machine.** a later change owns the table and the one function that writes a
//!   state. The provisioner asks it for `starting → idle` (`PiReady`) or `starting → error`
//!   (`ProvisioningFailed`) and emits the four step reports itself, because a step beginning is
//!   not a state change: its payload says `from: starting, to: starting, step: …`.

use std::sync::{Arc, Mutex, PoisonError};

use tokio::task::JoinHandle;

use crate::backend::{Backends, ProcessEnd, Provisioned, SessionBackend, Step};
use crate::clock;
use crate::db::Database;
use crate::events::{EventWriter, NewEvent};
use crate::exit::Exits;
use crate::sessions::state::{SESSION_STATUS, SessionStatus, Transitioner, Trigger};
use crate::sessions::{Session, Status};

/// The provisioner's handles: where to run steps, where the row is, and where events go.
///
/// `Clone` because the create handler builds one per accepted session and hands it to a task; the
/// four fields are a backend value, one `Arc`, a sender and a cheap handle, so cloning copies no state.
#[derive(Debug, Clone)]
pub struct Provisioner {
    backend: Backends,
    database: Arc<Mutex<Database>>,
    events: EventWriter,
    /// What a finished agent process's exit means, once the pi step has left one running.
    ///
    /// Built here from the row database rather than passed in: the data root is the directory the database
    /// lives in ([`Exits::for_database`]), so the two handles this runner already has are all an exit's
    /// consequences need.
    exits: Exits,
    /// What the success ending records when the run is a **recovery** rather than a request.
    ///
    /// `None` — every provisioner a create or a resume builds — settles to a bare `idle`. A startup
    /// recovery of a row that was `running` settles to `idle` **with** `interrupted`, so the reason survives
    /// the re-provision and the row the operator comes back to says what happened to the turn. The failure
    /// ending ignores it: a recovery that failed is `error` with the failure's own sentence, which is more
    /// useful than the crash's word.
    settle_reason: Option<String>,
}

impl Provisioner {
    pub fn new(backend: Backends, database: Arc<Mutex<Database>>, events: EventWriter) -> Self {
        let exits = Exits::for_database(Arc::clone(&database), events.clone());
        Self {
            backend,
            database,
            events,
            exits,
            settle_reason: None,
        }
    }

    /// The same provisioner, settling with `reason`.
    ///
    /// The reconcile is the only caller: a row that was `running` when the process died comes back `idle`
    /// with `interrupted` as the row's reason, which is the badge the operator reads and the one fact the
    /// page has about the turn that was cut.
    pub fn with_settle_reason(mut self, reason: Option<&str>) -> Self {
        self.settle_reason = reason.map(str::to_string);
        self
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
            // Somebody else may have ended this session's provisioning: a delete moves the row to
            // `deleting` whatever state it was in, and this task must then stop building
            // resources for a session that is going away. Nothing is undone — the delete's
            // `SessionBackend::delete` is what removes them, and undoing here would race it. A
            // step already in flight when the row moved is the to make safe.
            if !self.still_starting(&session.id) {
                tracing::info!(
                    target: "holosmith::provisioner",
                    session = %session.id,
                    step = step.as_str(),
                    "the session left starting, so provisioning stopped"
                );
                return;
            }
            self.announce(&session, step).await;
            match self.backend.provision(&session, step).await {
                Ok(provisioned) => {
                    // Only what this provision made is this provision's to take back: a worktree step that
                    // adopted the session's existing worktree leaves it (and the commits on it) alone when
                    // a later step fails. See `Provisioned::undoable`.
                    if provisioned.undoable() {
                        completed.push(step);
                    }
                    self.record(&session, provisioned);
                    // A fresh container has not been asked to stop (spec item 6). The flag is
                    // cleared here rather than by the backend, which may not write a row (ADR-0019), and
                    // here rather than at the resume: this is the one step that makes a *new* container,
                    // and a flag left over from the last one would call its first crash an expected stop.
                    if step == Step::Container {
                        self.clear_expected_stop(&session);
                    }
                    // The last step left a process running, and its exit is the server's to record: the
                    // backend hands it over and a task of this one's watching calls `Exits::record` when
                    // it arrives (ADR-0038). Done *after* the step reported success, so a stack that
                    // never became ready has no watcher.
                    if step == Step::Pi {
                        self.supervise(&session).await;
                    }
                }
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

    /// Watch the agent process the pi step left running, and record how it ends.
    ///
    /// A task of its own rather than a branch here: this run is over once the session is `idle`, and an
    /// agent process outlives it by hours. The backend answers `None` when it has no process to supervise
    /// (the stub, and a Docker session whose launch never happened), which is not a failure.
    ///
    /// The exit's *meaning* is [`crate::exit`]'s three questions and the backend's fourth ("is this exit
    /// still about the current container?"): this task only routes the answer.
    async fn supervise(&self, session: &Session) {
        let Some(watcher) = self.backend.supervise(session).await else {
            return;
        };
        let exits = self.exits.clone();
        let session = session.clone();
        tokio::spawn(async move {
            match watcher.wait().await {
                ProcessEnd::Exited { code, stderr } => exits.record(&session, code, &stderr).await,
                ProcessEnd::NotCurrent => tracing::debug!(
                    target: "holosmith::provisioner",
                    session = %session.id,
                    "the agent process that ended is not the session's current one"
                ),
            }
        });
    }

    /// Write what a step made into the row, before the next step runs.
    ///
    /// A backend cannot write the row (ADR-0019), so a step that creates something the row names answers
    /// with it and this is the writer. The worktree step is the only one so far: its path and the commit
    /// its branch starts from are the row's `worktree` and `base_commit`, and every reader of a session's
    /// work — the changes sheet, the mounts and the delete's teardown — names one of
    /// them.
    ///
    /// A failed write is logged and provisioning carries on. The resource exists either way, so failing
    /// the session over a row that could not be written would undo work that succeeded; a row without the
    /// path is a reader's problem, and the next provision recomputes it from the naming rule anyway.
    fn record(&self, session: &Session, provisioned: Provisioned) {
        let Provisioned::Worktree {
            path, base_commit, ..
        } = provisioned
        else {
            return;
        };
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(error) = database
            .sessions()
            .set_worktree(&session.id, &path, &base_commit)
        {
            tracing::warn!(
                target: "holosmith::provisioner",
                session = %session.id,
                %error,
                "the worktree step's record was not written to the row"
            );
        }
    }

    /// Clear the expected-stop flag once the container step has made a new container.
    ///
    /// A failed write is logged and provisioning carries on, for [`Self::record`]'s reason: the container
    /// exists either way, and the price of a flag that was not cleared is a misclassified exit rather than
    /// a broken session.
    ///
    /// **The ordering this depends on, and how it is kept:** the *previous* container's exit must not be
    /// classified as a crash after this clear has reset the flag. It cannot be: a launch has a generation,
    /// and an exit whose generation is no longer the session's current one answers
    /// [`crate::backend::ProcessEnd::NotCurrent`] and is never classified (ADR-0039). So the
    /// ladder's immediate resume after rung two is safe without waiting for anything.
    fn clear_expected_stop(&self, session: &Session) {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(error) = database.sessions().set_expected_stop(&session.id, false) {
            tracing::warn!(
                target: "holosmith::provisioner",
                session = %session.id,
                %error,
                "the expected-stop flag was not cleared for the new container"
            );
        }
    }

    /// Whether `session_id` is still `starting`, which is the only state this task provisions.
    ///
    /// A read that fails answers `true`: an unreadable row is not evidence that somebody ended the
    /// session, and stopping on it would turn a transient lock into a session that never provisions.
    /// A row that is gone answers `false` — there is nothing left to provision.
    fn still_starting(&self, session_id: &str) -> bool {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        match database.sessions().get(session_id) {
            Ok(Some(session)) => session.status == Status::Starting,
            Ok(None) => false,
            Err(error) => {
                tracing::warn!(
                    target: "holosmith::provisioner",
                    session = session_id,
                    %error,
                    "the provisioner could not read the session"
                );
                true
            }
        }
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
    /// and emits the event in that order. `settle_reason` is the recovery's word, when there is one.
    async fn settle(&self, session: &Session) {
        self.transition(session, Trigger::PiReady, self.settle_reason.as_deref())
            .await;
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
