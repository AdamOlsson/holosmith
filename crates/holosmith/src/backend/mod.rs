//! What provisioning and the session lifecycle actually do, behind one trait (HS-011).
//!
//! Two backends exist from this story on: the in-process **stub backend** that runs the whole
//! lifecycle with no Docker, git or agent binary (ADR-0001), and the **Docker backend** that
//! HS-024 fills in. `runtime::Backend` is which one *this run* uses — a choice — and
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

use serde::Serialize;

pub use docker::DockerBackend;
pub use stub::{Call, Recorded, StubBackend};

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
    /// The backend does not implement this operation in this build: the Docker backend before
    /// HS-024, and nothing else. Carrying the step keeps the reason specific.
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

/// The eight operations the story names, plus the per-step undo a failed provisioning needs.
///
/// `async fn` in a trait, which is not object-safe: the crate never holds a `dyn SessionBackend`
/// (it holds [`Backends`]), and there is no implementation outside this crate, so the
/// alternative — an `async_trait` dependency — would buy a virtual call this code cannot make.
#[allow(async_fn_in_trait)]
pub trait SessionBackend {
    /// Run one provisioning step for `session`.
    async fn provision(&self, session: &Session, step: Step) -> Result<(), BackendError>;

    /// Undo one completed provisioning step, leaving no partial resource behind.
    ///
    /// Called in the reverse of the order the steps ran, and only for the steps that completed.
    async fn undo(&self, session: &Session, step: Step) -> Result<(), BackendError>;

    /// Start a turn: send `prompt` to the agent.
    async fn start_turn(&self, session: &Session, prompt: &str) -> Result<(), BackendError>;

    /// Add guidance to the turn that is running.
    async fn steer(&self, session: &Session, text: &str) -> Result<(), BackendError>;

    /// End the running turn early.
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
    /// The backend `runtime::Backend` selects.
    pub fn for_run(backend: Backend) -> Self {
        match backend {
            Backend::Stub => Self::Stub(StubBackend::default()),
            Backend::Docker => Self::Docker(DockerBackend),
        }
    }

    /// The backend's own word: what a log line and a reason call it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stub(_) => "stub",
            Self::Docker(_) => "docker",
        }
    }
}

impl Default for Backends {
    /// The default run's backend, which is the Docker backend (ADR-0001: the stub is opt-in).
    fn default() -> Self {
        Self::for_run(Backend::default())
    }
}

impl SessionBackend for Backends {
    async fn provision(&self, session: &Session, step: Step) -> Result<(), BackendError> {
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

    async fn start_turn(&self, session: &Session, prompt: &str) -> Result<(), BackendError> {
        match self {
            Self::Stub(backend) => backend.start_turn(session, prompt).await,
            Self::Docker(backend) => backend.start_turn(session, prompt).await,
        }
    }

    async fn steer(&self, session: &Session, text: &str) -> Result<(), BackendError> {
        match self {
            Self::Stub(backend) => backend.steer(session, text).await,
            Self::Docker(backend) => backend.steer(session, text).await,
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
            let backend = Backends::for_run(choice);
            assert_eq!(backend.as_str(), word);
        }
        assert_eq!(Backends::default().as_str(), "docker");
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
