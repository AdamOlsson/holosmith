//! The turn runner: the background work one prompt becomes (HS-015), and the deadline that ends a turn
//! which will not end itself (HS-022).
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
//!
//! **The turn timeout (HS-022) is this task's own deadline.** Spec item 13 gives a turn that never settles
//! a bound (`turn_timeout_min`), and the clock belongs here rather than in a sweeper because the row keeps
//! no per-turn timestamp: `last_active_at` moves on a steer, so a column-based watcher would let a steered
//! turn run for ever. The deadline starts when [`Turn::run`] does and dies with the task, so a turn that
//! settles has no clock to reset. When it fires, the ladder climbs with [`TURN_TIMEOUT`] — the same two
//! rungs an operator's abort uses (ADR-0020) — while the turn is still polled, because rung 1 working *is*
//! the turn's own [`TurnEnd::Aborted`].

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::backend::{BackendError, Backends, SessionBackend, TurnEnd};
use crate::clock;
use crate::db::Database;
use crate::events::EventWriter;
use crate::ladder::Ladder;
use crate::sessions::Session;
use crate::sessions::activity;
use crate::sessions::state::{StateError, Transitioner, Trigger};

/// The reason a turn that ended because of an abort records on `running -> idle`.
///
/// The event's payload has no trigger field (ADR-0016), so the reason is the only place a client
/// can read *how* a turn ended: `abort_completed` here, [`crate::ladder::FORCED_ABORT`] when the
/// ladder had to kill the container. Spelling the trigger's own word keeps one vocabulary.
pub const ABORT_COMPLETED: &str = "abort_completed";

/// The reason a turn that ran past `turn_timeout_min` records (spec item 13).
///
/// The fifth reason word, after `forced_abort`, `deleted`, `evicted` and `idle_stop`, and the one a client
/// reads to tell "the agent is wedged" from "I stopped it" and "it stopped itself".
pub const TURN_TIMEOUT: &str = "turn_timeout";

/// The deadline `turn_timeout_min` asks for, or `None` when the key is 0.
///
/// `0` is the off switch (spec item 13's table, and the same reading HS-021's `idle_stop_min = 0` has):
/// no timer is built at all, rather than a timer that never fires. A key the operator turned off should
/// cost nothing.
pub fn timeout(turn_timeout_min: u64) -> Option<Duration> {
    (turn_timeout_min > 0).then(|| clock::minutes(turn_timeout_min))
}

/// The turn runner's handles: where a turn runs, where the row is, and where its frames go.
///
/// `Clone` for the same reason [`crate::provisioner::Provisioner`] is: the prompt handler builds one
/// per accepted prompt and hands it to a task, and the fields are a backend value, two `Arc`s and a
/// sender, so cloning copies no state.
#[derive(Debug, Clone)]
pub struct Turn {
    backend: Backends,
    database: Arc<Mutex<Database>>,
    events: EventWriter,
    /// `abort_grace`, handed to the ladder a turn past its deadline climbs.
    grace: Duration,
    /// `turn_timeout_min` as a duration, or `None` when the key is 0: no deadline is built at all.
    timeout: Option<Duration>,
}

impl Turn {
    pub fn new(
        backend: Backends,
        database: Arc<Mutex<Database>>,
        events: EventWriter,
        grace: Duration,
        timeout: Option<Duration>,
    ) -> Self {
        Self {
            backend,
            database,
            events,
            grace,
            timeout,
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
    ///
    /// The deadline starts here and dies with this task, which is what "the timeout is per turn, not per
    /// session" means: a turn that settles never has a clock to reset, and a steer cannot extend one
    /// (the deadline was fixed when the turn was). A `None` timeout builds no timer at all.
    pub async fn run(self, session: Session, prompt: String) {
        self.touch(&session.id);
        let turn = self.backend.start_turn(&session, &prompt, &self.events);
        tokio::pin!(turn);
        let Some(limit) = self.timeout else {
            let ended = turn.await;
            self.finished(&session, ended, None).await;
            return;
        };
        tokio::select! {
            ended = &mut turn => self.finished(&session, ended, None).await,
            () = tokio::time::sleep(limit) => {
                self.past_the_deadline(&session, limit, turn).await
            }
        }
    }

    /// The deadline passed: hand the ladder a task of its own and keep awaiting the turn.
    ///
    /// **The ladder is spawned, not branched on.** A `select!` branch would drop the climb the moment the
    /// turn answered, and a climb dropped after rung 2 has moved the row — between the row's `stopped` and
    /// the resume that follows it — would leave the session stopped with nobody left to bring it back,
    /// which is spec item 16 §2's "and the server resumes it at once" broken outright. `Ladder::spawn` is
    /// the abort handler's own entry point (HS-016), so a timeout and an operator's abort reach the
    /// container through one routine and race the runner in exactly the way ADR-0020 recorded.
    ///
    /// The turn is still polled here, because rung 1 working *is* [`TurnEnd::Aborted`]: the ladder asks the
    /// agent, and the agent's answer arrives as this future resolving. Nothing is cancelled and no server
    /// token is invented: the backend still decides how its turn ended.
    async fn past_the_deadline(
        &self,
        session: &Session,
        limit: Duration,
        turn: Pin<&mut impl Future<Output = Result<TurnEnd, BackendError>>>,
    ) {
        tracing::info!(
            target: "holosmith::turn",
            session = %session.id,
            timeout_s = limit.as_secs(),
            "the turn ran past its timeout; the ladder is ending it"
        );
        self.ladder().spawn(session.clone(), TURN_TIMEOUT);
        let ended = turn.await;
        self.finished(session, ended, Some(TURN_TIMEOUT)).await;
    }

    /// How the turn ended, and what the row and the log record about it.
    ///
    /// `reason` is `Some` only when the *runner* ended the turn for a reason of its own — the timeout —
    /// because an ordinary turn's ending is the agent's and an ordinary abort's reason is
    /// [`ABORT_COMPLETED`].
    async fn finished(
        &self,
        session: &Session,
        ended: Result<TurnEnd, BackendError>,
        reason: Option<&'static str>,
    ) {
        match ended {
            Ok(TurnEnd::Settled) => {
                // A turn that finished after its deadline and instead of stopping records the reason too: it
                // was asked to end because of the bound, and "the reason reaches the transcript" is what the
                // story asks for. On the ordinary path `reason` is `None` and the row records nothing.
                self.transition(session, Trigger::AgentSettled, reason)
                    .await;
                self.touch(&session.id);
            }
            Ok(TurnEnd::Aborted) => {
                self.abort_ended(session, reason.unwrap_or(ABORT_COMPLETED))
                    .await;
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
                self.transition(session, Trigger::UnexpectedExit, Some(&reason))
                    .await;
            }
        }
    }

    /// The abort ending: `running -> idle` on `reason`, unless the ladder got there first.
    ///
    /// Two tasks can end one turn (HS-016, ADR-0020): the ladder's second rung exists exactly for a
    /// turn that did not stop, so it applies `running -> stopped` and then stops the backend, which
    /// is what finally makes this turn answer [`TurnEnd::Aborted`]. The row is therefore often no
    /// longer `running` by the time this runs, and the table refusing the row is the ordinary
    /// outcome of the two owners rather than a bug — so it is a `debug` line here, and only here.
    async fn abort_ended(&self, session: &Session, reason: &'static str) {
        match self
            .transitioner()
            .apply(&session.id, Trigger::AbortCompleted, Some(reason))
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

    /// An abort ladder wired to this runner's handles, for a turn that ran past its deadline.
    ///
    /// The grace is the one `AppState` gave this runner when it built it — the same config field the abort
    /// handler reads — so a timeout cannot be given a shorter or longer forced rung than an operator's
    /// abort. The ladder's own constructor rather than a copy of its fields, so the two rungs live once.
    fn ladder(&self) -> Ladder {
        Ladder::new(
            self.backend.clone(),
            Arc::clone(&self.database),
            self.events.clone(),
            self.grace,
        )
    }

    /// The one state function, wired to this runner's two handles.
    fn transitioner(&self) -> Transitioner {
        Transitioner::new(Arc::clone(&self.database), self.events.clone())
    }
}
