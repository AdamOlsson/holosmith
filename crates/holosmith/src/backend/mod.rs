//! What provisioning and the session lifecycle actually do, behind one trait (HS-011).
//!
//! Two backends exist from this story on: the in-process **stub backend** that runs the whole
//! lifecycle with no Docker, git or agent binary (ADR-0001), and the **Docker backend** that
//! HS-025 fills in. `runtime::Backend` is which one *this run* uses — a choice — and
//! [`SessionBackend`] is the behaviour both must provide. The two are different things with
//! similar names, so the selector stays in [`crate::runtime`] and the trait lives here.
//!
//! Every step of provisioning is one call, not one loop: the provisioner decides the order and
//! reports progress, and a backend decides what one step *means*. The alternative — a single
//! `provision()` that ran all four — would put the progress reporting inside every backend and
//! give the Docker backend a reason to emit its own events.

mod docker;
mod stub;

use std::borrow::Cow;
use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

pub use docker::DockerBackend;
pub use stub::{Call, Recorded, StubBackend};

use crate::events::EventWriter;
use crate::runtime::Backend;
use crate::sessions::Session;

/// One stage of provisioning, in the order the spec names them (item 7).
///
/// The word is the payload's `step` value, and this is the only spelling of it: the enum
/// serialises through [`Self::as_str`] rather than through a derive, so the wire word, the log
/// word and the test's expectation cannot drift (the `Status` precedent, ADR-0011).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Step {
    /// `<projects>/.worktrees/<id>` and the branch `agent/<id>`.
    Worktree,
    /// The session's private network and its two proxies.
    Network,
    /// `docker run … s_<id>`.
    Container,
    /// The agent process inside the container is ready for a turn.
    Pi,
}

impl Step {
    /// Every step, in the order a provisioner runs them.
    pub const ALL: [Step; 4] = [Step::Worktree, Step::Network, Step::Container, Step::Pi];

    /// The text the event payload and the log carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worktree => "worktree",
            Self::Network => "network",
            Self::Container => "container",
            Self::Pi => "pi",
        }
    }

    /// The step a word names, if it names one. Used by the config's failure injection and by
    /// tests; the payload's `step` is written from `as_str`, never parsed back in production.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|step| step.as_str() == text)
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The one spelling: a derive with `rename_all` would be a second place the four words live.
impl Serialize for Step {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Why a backend could not do something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    /// The backend does not implement this operation in this build: the Docker backend until the
    /// story that fills the operation in (HS-025 and after), and nothing else. Carrying the step keeps
    /// the reason specific.
    Unsupported {
        step: Option<Step>,
        detail: Cow<'static, str>,
    },
    /// The operation ran and failed, for `reason`. The text reaches the row's `status_reason`
    /// and the `session.status` event, so it is written for the operator.
    Failed { reason: String },
}

impl BackendError {
    /// A step the backend cannot run yet.
    pub fn unsupported_step(step: Step, detail: &'static str) -> Self {
        Self::Unsupported {
            step: Some(step),
            detail: Cow::Borrowed(detail),
        }
    }

    /// An operation the backend cannot run yet, on a backend whose word is `backend`.
    ///
    /// The sentence is owned rather than leaked: this path is reached only by a backend that does
    /// not implement an operation, and a leak per call would be a slow, permanent growth the day
    /// an endpoint starts calling the lifecycle methods of such a backend.
    pub fn unsupported(backend: &str, operation: &str) -> Self {
        Self::Unsupported {
            step: None,
            detail: Cow::Owned(format!(
                "the {backend} backend does not implement {operation} yet"
            )),
        }
    }

    /// A failure with an operator-readable reason.
    pub fn failed(reason: impl Into<String>) -> Self {
        Self::Failed {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported {
                step: Some(step),
                detail,
            } => write!(f, "the {step} step: {detail}"),
            Self::Unsupported { step: None, detail } => f.write_str(detail.as_ref()),
            Self::Failed { reason } => f.write_str(reason),
        }
    }
}

impl std::error::Error for BackendError {}

/// How a turn ended, as the backend saw it (HS-016).
///
/// The backend reports what happened to the turn it was asked to run; the runner still chooses the
/// table row (ADR-0019: the backend emits, the server decides). Two answers are enough for both
/// rows `running` has: [`Self::Settled`] is the agent finishing on its own, [`Self::Aborted`] is a
/// turn that stopped because [`SessionBackend::abort`] or [`SessionBackend::stop`] cut it short — a
/// forced stop lands on the same table row as an abort, because the caller ascends the ladder and
/// the row is already `stopped` by then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    /// The agent finished everything for this turn.
    Settled,
    /// Something stopped the turn before the agent finished.
    Aborted,
}

/// What one provisioning step made that the row records (HS-025).
///
/// A backend **emits** frames and cannot write the row (ADR-0019), so a step that creates something the
/// row names answers with it and the provisioner writes it. Three of the four steps record nothing, and
/// that is the variant they answer: there is no "no worktree" worth writing, because the row's columns
/// already say what the first provision left there.
///
/// An enum rather than a struct of `Option` fields, so a step that records something *cannot* answer
/// half of it: the worktree's path and its base commit are one fact about one worktree, and `ahead`
/// (HS-061) is measured from the second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provisioned {
    /// The step built nothing the row names.
    Nothing,
    /// The worktree step made this session's worktree: where it is, and the commit its branch starts
    /// from. The row's `worktree` and `base_commit` columns are these two values (spec item 1).
    Worktree {
        /// `<projects>/.worktrees/<id>`, as the row stores it and as HS-027 mounts it at `/work`.
        path: String,
        /// The commit `agent/<id>` starts from.
        base_commit: String,
        /// `true` when this provision **created** the worktree, `false` when it adopted one that was
        /// already there. It is the difference between work this step owns and work it found, which is
        /// what [`Self::undoable`] answers.
        created: bool,
    },
}

impl Provisioned {
    /// Whether the provisioner has anything to undo for this step.
    ///
    /// A step that completed is undone when a later step fails (ADR-0015: a provision leaves no partial
    /// resource behind), with one exception: **a worktree the step adopted is not its to take back.** The
    /// files and the commits on it were the session's before this provision ran — a resume arrives with
    /// them (ADR-0022), and the step re-attaches rather than recreates — so deleting them because the
    /// *network* step refused would throw away an agent's work to tidy up after a step that never made
    /// anything.
    ///
    /// [`Self::Nothing`] is undoable: a step that built nothing outside the process still ran, and the
    /// stub's teardown is recorded (and asserted) so a backend that does build something has a shape to
    /// fill.
    pub fn undoable(&self) -> bool {
        match self {
            Self::Nothing => true,
            Self::Worktree { created, .. } => *created,
        }
    }
}

/// The eight operations the story names, plus the per-step undo a failed provisioning needs.
///
/// `async fn` in a trait, which is not object-safe: the crate never holds a `dyn SessionBackend`
/// (it holds [`Backends`]), and there is no implementation outside this crate, so the
/// alternative — an `async_trait` dependency — would buy a virtual call this code cannot make.
#[allow(async_fn_in_trait)]
pub trait SessionBackend {
    /// Run one provisioning step for `session`, and answer what it made that the row records.
    ///
    /// Most steps record nothing and answer [`Provisioned::Nothing`]. The worktree step answers the path
    /// and the commit its branch starts from, because the provisioner is what writes a row (ADR-0019) —
    /// the backend has no `SessionRepository` and must not grow one.
    async fn provision(&self, session: &Session, step: Step) -> Result<Provisioned, BackendError>;

    /// Undo one completed provisioning step, leaving no partial resource behind.
    ///
    /// Called in the reverse of the order the steps ran, and only for the steps that completed.
    async fn undo(&self, session: &Session, step: Step) -> Result<(), BackendError>;

    /// Start a turn: send `prompt` to the agent, and report what the turn produces as events.
    ///
    /// The backend **emits** a turn's frames and never decides a state (D6): it is handed the
    /// writer, because the frames are the backend's own output, and it has no `Transitioner`, so a
    /// state change stays the server's. The stub scripts a turn and HS-032 pumps pi's RPC into the
    /// same writer.
    ///
    /// The answer says **how** the turn ended (HS-016), which is the one thing the server cannot
    /// observe: `Settled` is the agent finishing, `Aborted` is an [`Self::abort`] or [`Self::stop`]
    /// cutting the turn short. It is not a state decision — the runner maps it onto the table
    /// (`agent_settled` or `abort_completed`) — but without it the two rows would be
    /// indistinguishable from the outside.
    async fn start_turn(
        &self,
        session: &Session,
        prompt: &str,
        events: &EventWriter,
    ) -> Result<TurnEnd, BackendError>;

    /// Add guidance to the turn that is running.
    async fn steer(&self, session: &Session, text: &str) -> Result<(), BackendError>;

    /// Drop the prompts the agent has queued but not started.
    ///
    /// The abort ladder's first step, and its own operation rather than part of [`Self::abort`],
    /// because the spec names two RPC calls in order (item 16 §1) and an agent whose queue survives
    /// an abort starts the next prompt the operator no longer wants.
    async fn clear_queue(&self, session: &Session) -> Result<(), BackendError>;

    /// End the running turn early.
    ///
    /// Returns once the agent has been told, not once the turn has stopped: the turn's own
    /// `start_turn` answers [`TurnEnd::Aborted`] when the agent gets there, and the ladder is what
    /// waits for it (HS-016, ADR-0020).
    async fn abort(&self, session: &Session) -> Result<(), BackendError>;

    /// Stop the container, keeping the worktree, the history and the events.
    async fn stop(&self, session: &Session) -> Result<(), BackendError>;

    /// Bring a stopped session back, with the same id.
    async fn resume(&self, session: &Session) -> Result<(), BackendError>;

    /// Remove everything the session owns.
    async fn delete(&self, session: &Session) -> Result<(), BackendError>;

    /// Record uncommitted work without touching the branch or the working tree.
    async fn snapshot(&self, session: &Session) -> Result<(), BackendError>;
}

/// The backend this run uses.
///
/// An enum rather than a `Box<dyn SessionBackend>`, because `async fn` in a trait is not
/// object-safe and every impl is in this crate; the implementation below is one `match` per
/// operation and adds no behaviour of its own.
#[derive(Debug, Clone)]
pub enum Backends {
    Stub(StubBackend),
    Docker(DockerBackend),
}

impl Backends {
    /// The backend `runtime::Backend` selects, in the machine's projects root.
    ///
    /// The projects root is the one machine setting the Docker backend needs to name anything at all:
    /// a session's worktree and repository both hang off it (HS-025). The stub needs no path and ignores
    /// it — it provisions nothing outside the process (ADR-0015).
    pub fn for_run(backend: Backend, projects_root: PathBuf) -> Self {
        match backend {
            Backend::Stub => Self::Stub(StubBackend::default()),
            Backend::Docker => Self::Docker(DockerBackend::new(projects_root)),
        }
    }

    /// The backend's own word: what a log line and a reason call it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stub(_) => "stub",
            Self::Docker(_) => "docker",
        }
    }

    /// Whether the project this create names may be mounted, and why not when it may not (HS-026).
    ///
    /// The question belongs to the backend that mounts a repository: a stub session has no repository at
    /// all, so it refuses nothing and every stub test and the gate's stub boot need no project on disk
    /// (ADR-0015). It is answered here rather than in the handler for that reason — one place knows which
    /// backend runs, and it is the one that already holds the projects root.
    pub fn refuse(&self, project: &str) -> Result<(), crate::worktree::Refusal> {
        match self {
            Self::Stub(_) => Ok(()),
            Self::Docker(backend) => backend.refuse_project(project),
        }
    }
}

impl SessionBackend for Backends {
    async fn provision(&self, session: &Session, step: Step) -> Result<Provisioned, BackendError> {
        match self {
            Self::Stub(backend) => backend.provision(session, step).await,
            Self::Docker(backend) => backend.provision(session, step).await,
        }
    }

    async fn undo(&self, session: &Session, step: Step) -> Result<(), BackendError> {
        match self {
            Self::Stub(backend) => backend.undo(session, step).await,
            Self::Docker(backend) => backend.undo(session, step).await,
        }
    }

    async fn start_turn(
        &self,
        session: &Session,
        prompt: &str,
        events: &EventWriter,
    ) -> Result<TurnEnd, BackendError> {
        match self {
            Self::Stub(backend) => backend.start_turn(session, prompt, events).await,
            Self::Docker(backend) => backend.start_turn(session, prompt, events).await,
        }
    }

    async fn steer(&self, session: &Session, text: &str) -> Result<(), BackendError> {
        match self {
            Self::Stub(backend) => backend.steer(session, text).await,
            Self::Docker(backend) => backend.steer(session, text).await,
        }
    }

    async fn clear_queue(&self, session: &Session) -> Result<(), BackendError> {
        match self {
            Self::Stub(backend) => backend.clear_queue(session).await,
            Self::Docker(backend) => backend.clear_queue(session).await,
        }
    }

    async fn abort(&self, session: &Session) -> Result<(), BackendError> {
        match self {
            Self::Stub(backend) => backend.abort(session).await,
            Self::Docker(backend) => backend.abort(session).await,
        }
    }

    async fn stop(&self, session: &Session) -> Result<(), BackendError> {
        match self {
            Self::Stub(backend) => backend.stop(session).await,
            Self::Docker(backend) => backend.stop(session).await,
        }
    }

    async fn resume(&self, session: &Session) -> Result<(), BackendError> {
        match self {
            Self::Stub(backend) => backend.resume(session).await,
            Self::Docker(backend) => backend.resume(session).await,
        }
    }

    async fn delete(&self, session: &Session) -> Result<(), BackendError> {
        match self {
            Self::Stub(backend) => backend.delete(session).await,
            Self::Docker(backend) => backend.delete(session).await,
        }
    }

    async fn snapshot(&self, session: &Session) -> Result<(), BackendError> {
        match self {
            Self::Stub(backend) => backend.snapshot(session).await,
            Self::Docker(backend) => backend.snapshot(session).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_steps_keep_the_specs_order_and_their_word() {
        let words: Vec<&str> = Step::ALL.iter().map(|step| step.as_str()).collect();
        assert_eq!(words, ["worktree", "network", "container", "pi"]);
    }

    #[test]
    fn every_step_parses_back_from_its_word() {
        for step in Step::ALL {
            assert_eq!(Step::parse(step.as_str()), Some(step));
            assert_eq!(step.to_string(), step.as_str());
            assert_eq!(
                serde_json::to_string(&step).expect("a step is one JSON string"),
                format!("\"{}\"", step.as_str())
            );
        }
        for text in ["", "Worktree", "worktree ", "docker", "pi ready"] {
            assert_eq!(Step::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_for_run_backend_carries_the_word_it_was_chosen_by() {
        for (choice, word) in [(Backend::Stub, "stub"), (Backend::Docker, "docker")] {
            let backend = Backends::for_run(choice, PathBuf::from("/srv/holosmith/projects"));
            assert_eq!(backend.as_str(), word);
        }
        assert_eq!(
            Backends::for_run(Backend::default(), PathBuf::from("/srv/holosmith/projects"))
                .as_str(),
            "docker",
            "the default run's backend is the Docker one (ADR-0001: the stub is opt-in)"
        );
    }

    #[test]
    fn an_unsupported_step_says_which_step_it_was() {
        let error = BackendError::unsupported_step(Step::Network, "no Docker here");
        assert_eq!(error.to_string(), "the network step: no Docker here");
        let error = BackendError::unsupported("docker", "provision");
        assert_eq!(
            error.to_string(),
            "the docker backend does not implement provision yet"
        );
        assert!(matches!(
            error,
            BackendError::Unsupported { step: None, .. }
        ));
        assert_eq!(
            BackendError::failed("the image is missing").to_string(),
            "the image is missing"
        );
    }
}
