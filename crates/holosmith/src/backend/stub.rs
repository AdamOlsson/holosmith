//! The stub backend: the whole lifecycle with no Docker, no git and no agent process
//! (ADR-0001).
//!
//! Its job is to make the API, the event stream and the state machine exercisable before any
//! container exists. So it creates nothing: a provisioning step is a delay and a note, and the
//! lifecycle calls record what they were asked to do and answer `Ok`. That is not a stub of the
//! *contract* — it is the contract, performed by a backend whose resources happen to be empty,
//! which is why the recording is what a test asserts against.
//!
//! The delay is the whole of "a slow start is legible": `serve --backend stub` uses
//! [`StubBackend::default`]'s per-step delay so a human can watch the four steps arrive, and a
//! test chooses milliseconds. It is a constructor argument and not a config key, because it
//! paces a stub rather than describing the machine (ADR-0006's table is machine settings).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::{BackendError, SessionBackend, Step};
use crate::sessions::Session;

/// How long one step takes under `serve --backend stub`: long enough that the four events are
/// four visible moments on a phone, short enough that a session is idle in under a second.
pub const DEFAULT_STEP_DELAY: Duration = Duration::from_millis(200);

/// One thing a caller asked the stub to do, in the order it was asked.
///
/// Denying (`Denied`) fails the call; [`StubBackend::failing_at`] is the provisioning form of it
/// and the one the story asks for. The rest are recorded so a test can prove an operation was
/// reached, which is what a backend with no resources can offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Provision(Step),
    Undo(Step),
    StartTurn,
    Steer,
    Abort,
    Stop,
    Resume,
    Delete,
    Snapshot,
}

/// What one call carried, for the calls whose arguments a test cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub call: Call,
    /// The session the call named.
    pub session: String,
    /// The turn's text: the prompt for [`Call::StartTurn`], the guidance for [`Call::Steer`],
    /// and empty otherwise.
    pub text: String,
}

/// The in-process backend. `Clone` shares the one recording, so a test keeps a handle after
/// handing a copy to the app.
#[derive(Debug, Clone)]
pub struct StubBackend {
    state: Arc<StubState>,
}

#[derive(Debug)]
struct StubState {
    step_delay: Duration,
    fail_at: Option<Step>,
    recorded: Mutex<Vec<Recorded>>,
}

impl Default for StubBackend {
    fn default() -> Self {
        Self::new(DEFAULT_STEP_DELAY)
    }
}

impl StubBackend {
    /// A stub whose every step takes `step_delay`.
    pub fn new(step_delay: Duration) -> Self {
        Self {
            state: Arc::new(StubState {
                step_delay,
                fail_at: None,
                recorded: Mutex::new(Vec::new()),
            }),
        }
    }

    /// The same stub, told to fail the named step — and every call to it — with a reason a test
    /// can assert verbatim.
    ///
    /// This is the story's "the stub can be told to fail at a named step". It is a method and
    /// not a config key: nothing an operator runs should be able to sabotage provisioning, and
    /// the failure path is proved by a test rather than by a demo.
    pub fn failing_at(mut self, step: Step) -> Self {
        Arc::get_mut(&mut self.state)
            .expect("a stub that has not been cloned yet is mutable")
            .fail_at = Some(step);
        self
    }

    /// The per-step delay this stub was built with.
    pub fn step_delay(&self) -> Duration {
        self.state.step_delay
    }

    /// Everything asked of this stub so far, in order.
    pub fn recorded(&self) -> Vec<Recorded> {
        self.state
            .recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The calls alone, in order: the shape most assertions want.
    pub fn calls(&self) -> Vec<Call> {
        self.recorded()
            .into_iter()
            .map(|entry| entry.call)
            .collect()
    }

    /// Note one call and answer the failure if this is the step the stub was told to fail.
    fn note(&self, call: Call, session: &Session, text: &str) -> Result<(), BackendError> {
        self.state
            .recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Recorded {
                call,
                session: session.id.clone(),
                text: text.to_string(),
            });
        match (call, self.state.fail_at) {
            (Call::Provision(step), Some(fail_at)) if step == fail_at => Err(BackendError::failed(
                format!("the stub backend was told to fail at the {step} step"),
            )),
            _ => Ok(()),
        }
    }
}

impl SessionBackend for StubBackend {
    async fn provision(&self, session: &Session, step: Step) -> Result<(), BackendError> {
        // Recorded before the wait, so a test can observe a step in flight — and so the failure
        // is ordered after the delay a real step would have spent: a stub that failed instantly
        // would hide a provisioner that assumed the step had taken time.
        let outcome = self.note(Call::Provision(step), session, "");
        tokio::time::sleep(self.state.step_delay).await;
        outcome
    }

    async fn undo(&self, session: &Session, step: Step) -> Result<(), BackendError> {
        // No delay: undoing nothing is nothing to wait for.
        self.note(Call::Undo(step), session, "")
    }

    async fn start_turn(&self, session: &Session, prompt: &str) -> Result<(), BackendError> {
        self.note(Call::StartTurn, session, prompt)
    }

    async fn steer(&self, session: &Session, text: &str) -> Result<(), BackendError> {
        self.note(Call::Steer, session, text)
    }

    async fn abort(&self, session: &Session) -> Result<(), BackendError> {
        self.note(Call::Abort, session, "")
    }

    async fn stop(&self, session: &Session) -> Result<(), BackendError> {
        self.note(Call::Stop, session, "")
    }

    async fn resume(&self, session: &Session) -> Result<(), BackendError> {
        self.note(Call::Resume, session, "")
    }

    async fn delete(&self, session: &Session) -> Result<(), BackendError> {
        self.note(Call::Delete, session, "")
    }

    async fn snapshot(&self, session: &Session) -> Result<(), BackendError> {
        self.note(Call::Snapshot, session, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::Status;

    fn session() -> Session {
        Session {
            id: "0123456789abcdef".to_string(),
            project: "holosmith".to_string(),
            model: "claude-sonnet-4.5".to_string(),
            thinking: "high".to_string(),
            worktree: String::new(),
            base_commit: None,
            status: Status::Starting,
            status_reason: None,
            client_key: Some("the-key".to_string()),
            network: false,
            created_at: 1_760_000_000,
            last_active_at: 1_760_000_000,
            stopped_at: None,
            usage_in: 0,
            usage_out: 0,
        }
    }

    /// A stub with no delay: these tests are about the recording, not about the clock.
    fn immediate() -> StubBackend {
        StubBackend::new(Duration::from_millis(1))
    }

    #[tokio::test]
    async fn every_operation_is_recorded_with_the_session_it_named() {
        let stub = immediate();
        let session = session();
        for step in Step::ALL {
            stub.provision(&session, step).await.expect("a step passes");
            stub.undo(&session, step).await.expect("an undo passes");
        }
        stub.start_turn(&session, "fix the parser")
            .await
            .expect("a prompt passes");
        stub.steer(&session, "and the lexer").await.expect("steer");
        stub.abort(&session).await.expect("abort");
        stub.stop(&session).await.expect("stop");
        stub.resume(&session).await.expect("resume");
        stub.delete(&session).await.expect("delete");
        stub.snapshot(&session).await.expect("snapshot");

        let recorded = stub.recorded();
        assert!(
            recorded.iter().all(|entry| entry.session == session.id),
            "a call lost its session"
        );
        let calls: Vec<Call> = recorded.iter().map(|entry| entry.call).collect();
        assert_eq!(
            calls,
            [
                Call::Provision(Step::Worktree),
                Call::Undo(Step::Worktree),
                Call::Provision(Step::Network),
                Call::Undo(Step::Network),
                Call::Provision(Step::Container),
                Call::Undo(Step::Container),
                Call::Provision(Step::Pi),
                Call::Undo(Step::Pi),
                Call::StartTurn,
                Call::Steer,
                Call::Abort,
                Call::Stop,
                Call::Resume,
                Call::Delete,
                Call::Snapshot,
            ]
        );
    }

    #[tokio::test]
    async fn the_prompt_and_the_steer_keep_their_text() {
        let stub = immediate();
        let session = session();
        stub.start_turn(&session, "fix the parser")
            .await
            .expect("a prompt");
        stub.steer(&session, "and the lexer")
            .await
            .expect("a steer");
        let recorded = stub.recorded();
        assert_eq!(recorded[0].text, "fix the parser");
        assert_eq!(recorded[1].text, "and the lexer");
        assert_eq!(recorded.len(), 2, "a lifecycle call invented a call");
    }

    #[tokio::test]
    async fn the_step_it_was_told_to_fail_fails_and_the_others_do_not() {
        let stub = immediate().failing_at(Step::Container);
        let session = session();
        stub.provision(&session, Step::Worktree)
            .await
            .expect("worktree is not the failing step");
        let error = stub
            .provision(&session, Step::Container)
            .await
            .expect_err("container is the failing step");
        assert_eq!(
            error.to_string(),
            "the stub backend was told to fail at the container step"
        );
        stub.provision(&session, Step::Pi)
            .await
            .expect("pi is not the failing step");
        // The failure is a provisioning failure only: undo must still work, or the provisioner
        // could not clean up the steps that did complete.
        stub.undo(&session, Step::Network)
            .await
            .expect("undo is never the failing step");
    }

    #[tokio::test]
    async fn a_step_takes_the_delay_it_was_built_with() {
        let stub = StubBackend::new(Duration::from_millis(60));
        assert_eq!(stub.step_delay(), Duration::from_millis(60));
        assert_eq!(
            StubBackend::default().step_delay(),
            DEFAULT_STEP_DELAY,
            "serve's stub paces itself with DEFAULT_STEP_DELAY"
        );
        let started = std::time::Instant::now();
        stub.provision(&session(), Step::Worktree)
            .await
            .expect("a step passes");
        assert!(
            started.elapsed() >= Duration::from_millis(60),
            "the step returned before its delay: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_clone_shares_the_one_recording() {
        let stub = immediate();
        let handed_over = stub.clone();
        handed_over
            .provision(&session(), Step::Worktree)
            .await
            .expect("a step passes");
        assert_eq!(stub.calls(), [Call::Provision(Step::Worktree)]);
    }
}
