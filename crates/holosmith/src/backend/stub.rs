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
//! [`StubBackend::default`]'s pace so a human can watch the four provisioning steps arrive and then
//! watch a turn's frames arrive one by one, and a test chooses milliseconds. It is a constructor
//! argument and not a config key, because it paces a stub rather than describing the machine
//! (ADR-0006's table is machine settings).
//!
//! A turn is a *script*: the stub emits the frames ADR-0018 and HS-015 pin — a boundary, thinking and
//! text deltas on one message id, a tool call, usage and the closing boundary — so the API, the
//! stream and every rendering path of the session screen are exercisable with no agent process. The
//! words live in [`StubBackend::script`] and nowhere else.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use super::{BackendError, SessionBackend, Step};
use crate::clock;
use crate::events::{EventWriter, NewEvent};
use crate::sessions::Session;

/// The stub's pace: how long one provisioning step, and one frame of a turn, takes under
/// `serve --backend stub`. Long enough that the four provisioning events and a turn's dozen frames
/// are visible moments on a phone, short enough that a session is idle in a couple of seconds.
pub const DEFAULT_STEP_DELAY: Duration = Duration::from_millis(200);

/// The six kinds a turn's script emits, in the order [`StubBackend::script`] emits them. Three are
/// ADR-0018's (`turn.start`, `message.delta`, `turn.end`); `tool.start`, `tool.end` and
/// `message.end` are HS-015's payloads for the rendering paths HS-033 and HS-034 fill in.
const TURN_START: &str = "turn.start";
const MESSAGE_DELTA: &str = "message.delta";
const TOOL_START: &str = "tool.start";
const TOOL_END: &str = "tool.end";
const MESSAGE_END: &str = "message.end";
const TURN_END: &str = "turn.end";

/// The stub's turn counter, and the instant it was first asked to mint one.
///
/// A message id and a tool id must be unique **for the life of the session's event log**, which
/// outlives the process: the log is durable (ADR-0013) and a client folds the whole replay into one
/// block per message id, so a restart that started over at `msg-1` would append the next turn's
/// text into the previous turn's block. The counter alone therefore cannot be the id, and the stub
/// has no database handle to read a durable number from (it deliberately has no state). The
/// process's own start instant supplies the missing range: a restarted process mints a disjoint set
/// of ids.
static NEXT_TURN: AtomicU64 = AtomicU64::new(1);

fn minted_instant() -> u128 {
    static INSTANT: OnceLock<u128> = OnceLock::new();
    *INSTANT.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    })
}

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

    /// The stub's pace: the delay before each provisioning step and before each frame of a turn.
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

impl StubBackend {
    /// One turn's frames, in the order a browser sees them: the two boundaries, thinking and text on
    /// one message id, one tool call with its output, the message's end with its usage, and the
    /// closing boundary.
    ///
    /// The payloads are `serde_json`'s, so the words an event carries live once. `turn` is the
    /// counter's value: it names the message (`msg-<turn>`) and the tool call (`tool-<turn>`), so a
    /// second turn is a second block in the transcript.
    fn script(turn: u64) -> Vec<(&'static str, String)> {
        // `<process instant>-<turn>`: unique within the session's log (the counter) and disjoint
        // across restarts (the instant), which is the whole of what ADR-0018 asks of an id.
        let message = format!("msg-{}-{turn}", minted_instant());
        let tool = format!("tool-{}-{turn}", minted_instant());
        let delta = |kind: &str, text: &str| {
            json!({"message_id": message, "kind": kind, "text": text}).to_string()
        };
        vec![
            (TURN_START, json!({}).to_string()),
            (
                MESSAGE_DELTA,
                delta("thinking", "The prompt names the app shell."),
            ),
            (
                MESSAGE_DELTA,
                delta("thinking", "Its rules live in transcript.ts, not in the page."),
            ),
            (
                MESSAGE_DELTA,
                delta("thinking", "So the answer is a reading of the shell."),
            ),
            (
                MESSAGE_DELTA,
                delta("text", "The shell is the session screen and the list."),
            ),
            (MESSAGE_DELTA, delta("text", " Reading it now.")),
            (
                TOOL_START,
                json!({"tool_id": tool, "name": "read_file", "arguments": "src/main.ts"})
                    .to_string(),
            ),
            (
                TOOL_END,
                json!({
                    "tool_id": tool,
                    "name": "read_file",
                    "output": "1  // The app shell: two screens that share one header and one stream.",
                })
                .to_string(),
            ),
            (
                MESSAGE_DELTA,
                delta("text", "The composer posts the prompt for real now."),
            ),
            (MESSAGE_DELTA, delta("text", " Nothing else in it moved.")),
            (
                MESSAGE_END,
                json!({"message_id": message, "usage": {"in": 1200, "out": 340}}).to_string(),
            ),
            (TURN_END, json!({}).to_string()),
        ]
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

    async fn start_turn(
        &self,
        session: &Session,
        prompt: &str,
        events: &EventWriter,
    ) -> Result<(), BackendError> {
        // Recorded before the first frame, so a test can observe the call in flight as it can a
        // provisioning step.
        let outcome = self.note(Call::StartTurn, session, prompt);
        outcome?;
        for (kind, payload) in Self::script(NEXT_TURN.fetch_add(1, Ordering::Relaxed)) {
            tokio::time::sleep(self.state.step_delay).await;
            let event = NewEvent {
                session_id: session.id.clone(),
                kind: kind.to_string(),
                payload,
                created_at: clock::unix_now(),
            };
            // A failed append means the writer has stopped, which in a served process means the
            // process is going down. The loss is logged and the turn carries on, exactly as the
            // provisioner's step reports do: the turn *ran*, so failing it here would write a
            // session that finished its work into `error` because the process was leaving.
            if let Err(error) = events.append(event).await {
                tracing::error!(
                    target: "holosmith::backend::stub",
                    session = %session.id,
                    %error,
                    "a turn event was not queued"
                );
            }
        }
        Ok(())
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

    #[test]
    fn a_turns_ids_are_not_just_the_counter() {
        // Message and tool ids must stay unique for the life of a session's durable log, which
        // outlives this process: a restart that started over at `msg-1` would fold the next turn's
        // text into the previous turn's block. The process's own instant is what makes the two
        // ranges disjoint, so it must be part of each id — this test would fail on the first
        // version of the script, whose ids were `msg-<n>` and `tool-<n>`.
        let first = StubBackend::script(1);
        let second = StubBackend::script(2);
        let payload = |script: &[(&'static str, String)], kind: &str| {
            script
                .iter()
                .find(|(emitted, _)| *emitted == kind)
                .map(|(_, payload)| payload.clone())
                .expect("the script emits this kind")
        };
        let delta = |script: &[(&'static str, String)]| {
            serde_json::from_str::<serde_json::Value>(&payload(script, MESSAGE_DELTA))
                .expect("a delta payload is JSON")["message_id"]
                .as_str()
                .expect("a delta names a message")
                .to_string()
        };
        let message_one = delta(&first);
        let message_two = delta(&second);
        let prefix = format!("{}", minted_instant());
        assert_ne!(
            message_one, "msg-1",
            "the id is the counter alone, so a restart mints it again"
        );
        assert!(
            message_one.starts_with(&format!("msg-{prefix}-")),
            "the process instant is not part of the id: {message_one}"
        );
        assert_ne!(
            message_one, message_two,
            "two turns of one process share a name"
        );
        assert_ne!(prefix, "0", "the process instant was not read");
        let tool = payload(&first, TOOL_START);
        assert!(
            tool.contains(&format!("tool-{prefix}-1")),
            "the tool id is not minted the same way: {tool}"
        );
    }

    #[tokio::test]
    async fn every_operation_is_recorded_with_the_session_it_named() {
        let stub = immediate();
        let session = session();
        let (_scratch, events) = crate::events::scratch_writer();
        for step in Step::ALL {
            stub.provision(&session, step).await.expect("a step passes");
            stub.undo(&session, step).await.expect("an undo passes");
        }
        stub.start_turn(&session, "fix the parser", &events)
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
        let (_scratch, events) = crate::events::scratch_writer();
        stub.start_turn(&session, "fix the parser", &events)
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
