//! The turn runner: the background work one prompt becomes, and the deadline that ends a turn
//! which will not end itself.
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
//! - **This is not a state machine.** This module owns the table and the one function that writes a
//!   state; the runner asks it for `agent_settled` or `unexpected_exit` and writes nothing itself.
//!
//! Abort and steer belong to a later change: the script is cut short by the backend's own
//! `abort`/`stop`, and the ladder that asks for it is [`crate::ladder`].
//!
//! **The token budget is this task's second policy**, and it is here for the same reason the
//! deadline is: a policy that must *record* a reason owns the ending, and only the runner writes the
//! ending. `max_session_tokens` is a ceiling on what the session has spent (`usage_in + usage_out`, which
//! the event writer accumulates from every `message.end`), so the runner polls the row while the turn
//! runs and, when the tally reaches the ceiling, climbs the same ladder with [`BUDGET_EXCEEDED`]. The
//! climb is spawned and the turn keeps being awaited, exactly as the timeout's is — and the first policy
//! to fire owns the reason, because a second climb would abort a turn that is already ending.
//!
//! **The turn timeout is this task's own deadline.** Spec item 13 gives a turn that never settles
//! a bound (`turn_timeout_min`), and the clock belongs here rather than in a sweeper because the row keeps
//! no per-turn timestamp: `last_active_at` moves on a steer, so a column-based watcher would let a steered
//! turn run for ever. The deadline starts when [`Turn::run`] does and dies with the task, so a turn that
//! settles has no clock to reset. When it fires, the ladder climbs with [`TURN_TIMEOUT`] — the same two
//! rungs an operator's abort uses (ADR-0020) — while the turn is still polled, because rung 1 working *is*
//! the turn's own [`TurnEnd::Aborted`].

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::Instant;

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

/// The reason a turn that spent the session's whole budget records (spec item 13).
///
/// The sixth reason word, after `forced_abort`, `deleted`, `evicted`, `idle_stop` and `turn_timeout`,
/// and the one a client reads to tell "it ran out of money" from "I stopped it" and "it stopped itself".
pub const BUDGET_EXCEEDED: &str = "budget_exceeded";

/// How often the runner compares the session's spend with its ceiling.
///
/// A row read is one indexed statement through the mutex ADR-0012 introduced, so this is a cheap
/// question, and it is not a pace setting: the tally moves only when a `message.end` is committed, which
/// the writer does at most every 25 ms (ADR-0013), so a shorter poll would ask the same question twice
/// for one answer. It is also the reason a turn can settle over budget: a turn whose last frame is its
/// `message.end` can finish between two polls, and nothing is lost — the session's *next* turn starts
/// with the tally already at the ceiling and trips on its first poll.
const BUDGET_POLL: Duration = Duration::from_millis(100);

/// The event writer's own commit interval, so the second look's wait is derived rather than restated.
///
/// An event appended now is committed by the writer's next tick (ADR-0013), so a read one tick later sees
/// every frame the turn appended before it ended. `WriterOptions` is the one place that interval is a
/// number — a construction site with a shorter or longer one moves this with it, which is why this is a
/// function over the default rather than a literal that would silently disagree.
fn writer_tick() -> Duration {
    crate::events::WriterOptions::default().commit_interval
}

/// The deadline `turn_timeout_min` asks for, or `None` when the key is 0.
///
/// `0` is the off switch (spec item 13's table, and the same reading 's `idle_stop_min = 0` has):
/// no timer is built at all, rather than a timer that never fires. A key the operator turned off should
/// cost nothing.
pub fn timeout(turn_timeout_min: u64) -> Option<Duration> {
    (turn_timeout_min > 0).then(|| clock::minutes(turn_timeout_min))
}

/// Wait until `at`, or for ever when there is no clock.
///
/// A `select!` arm has to exist even when its key is off, and a sleep for a duration nobody asked for is
/// not the same thing as no timer: this makes "the key is unset" one arm that can never win, so the
/// branch's removal is a guard rather than a second `select!`.
async fn until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
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
    /// `max_session_tokens`, or `None` when the key is unset: no poll is built at all, and a session with
    /// no ceiling costs nothing to run.
    budget: Option<u64>,
}

impl Turn {
    pub fn new(
        backend: Backends,
        database: Arc<Mutex<Database>>,
        events: EventWriter,
        grace: Duration,
        timeout: Option<Duration>,
        budget: Option<u64>,
    ) -> Self {
        Self {
            backend,
            database,
            events,
            grace,
            timeout,
            budget,
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
    /// (the deadline was fixed when the turn was). The budget's poll is the other clock, and it asks the
    /// row rather than a task: "what has this session spent" is a column, not a timer.
    ///
    /// **Whichever policy fires first owns the turn's ending.** Both end a turn the same way — the ladder,
    /// asked from a task of its own — and the reason that reaches the row is the first one's. A second
    /// climb would ask a second abort of a turn that is already ending, and the row's one
    /// `status_reason` could only keep one of the words anyway. It is also why both arms are guarded on
    /// `fired`: after one has fired there is nothing left for the other to do but wait for the turn.
    pub async fn run(self, session: Session, prompt: String) {
        self.touch(&session.id);
        let turn = self.backend.start_turn(&session, &prompt, &self.events);
        tokio::pin!(turn);
        let deadline = self.timeout.map(|limit| Instant::now() + limit);
        let mut poll = Instant::now() + BUDGET_POLL;
        let mut fired: Option<&'static str> = None;
        loop {
            tokio::select! {
                ended = &mut turn => {
                    self.finished(&session, ended, fired).await;
                    return;
                }
                () = until(deadline), if fired.is_none() => {
                    // `until` answers only when there is a deadline; `None` is a future that never
                    // resolves, so this `let` is the arm's own proof rather than a default.
                    let Some(limit) = self.timeout else { continue };
                    tracing::info!(
                        target: "holosmith::turn",
                        session = %session.id,
                        timeout_s = limit.as_secs(),
                        "the turn ran past its timeout; the ladder is ending it"
                    );
                    fired = Some(TURN_TIMEOUT);
                    self.spawn_ladder(&session, TURN_TIMEOUT);
                }
                () = tokio::time::sleep_until(poll), if fired.is_none() && self.budget.is_some() => {
                    poll = Instant::now() + BUDGET_POLL;
                    if let (Some(spend), Some(ceiling)) = (self.over_budget(&session.id), self.budget) {
                        tracing::info!(
                            target: "holosmith::turn",
                            session = %session.id,
                            budget = ceiling,
                            spend,
                            "the turn spent the session's token budget; the ladder is ending it"
                        );
                        fired = Some(BUDGET_EXCEEDED);
                        self.spawn_ladder(&session, BUDGET_EXCEEDED);
                    }
                }
            }
        }
    }

    /// Hand the ladder a task of its own, so the climb outlives this `select!`.
    ///
    /// **The ladder is spawned, not branched on.** A `select!` branch would drop the climb the moment the
    /// turn answered, and a climb dropped after rung 2 has moved the row — between the row's `stopped` and
    /// the resume that follows it — would leave the session stopped with nobody left to bring it back,
    /// which is spec item 16 §2's "and the server resumes it at once" broken outright. `Ladder::spawn` is
    /// the abort handler's own entry point, so a policy's ending and an operator's abort reach the
    /// container through one routine and race the runner in exactly the way ADR-0020 recorded.
    ///
    /// The turn is still polled by [`Self::run`]'s `select!`, because rung 1 working *is*
    /// [`TurnEnd::Aborted`]: the ladder asks the agent, and the agent's answer arrives as that future
    /// resolving. Nothing is cancelled and no server token is invented: the backend still decides how its
    /// turn ended.
    fn spawn_ladder(&self, session: &Session, reason: &'static str) {
        self.ladder().spawn(session.clone(), reason);
    }

    /// The label a turn that settled over its ceiling carries, or `None` when it settled under it
    ///
    ///
    /// **The second look narrows the window; it does not close it.** The tally is committed by the event
    /// writer on its own tick ([`writer_tick`]), so a turn that ends within a tick of its `message.end`
    /// answers before that frame is in the row, and the first look would read the spend from *before* the
    /// message that crossed the ceiling. Looking twice, one tick apart, is enough for every writer that
    /// commits on schedule — and not a barrier, so a writer that falls behind still answers from the older
    /// row. That is deliberate: a floor that is one frame late is the next turn's first poll's to catch,
    /// and blocking an ending on the writer would make every turn's settle wait for the disk.
    ///
    /// The cost is honest: this delay is paid on **every** ordinary turn of a budgeted session, because
    /// the first look is under the ceiling there. 25 ms against a turn is nothing, and a session with no
    /// ceiling — the default — pays none of it.
    async fn settled_over_budget(&self, session_id: &str) -> Option<&'static str> {
        self.budget?;
        if self.over_budget(session_id).is_some() {
            return Some(BUDGET_EXCEEDED);
        }
        tokio::time::sleep(writer_tick()).await;
        self.over_budget(session_id)
            .is_some()
            .then_some(BUDGET_EXCEEDED)
    }

    /// The session's spend, when it has reached the ceiling this runner was built with.
    ///
    /// `None` for every other answer, and for a row this runner cannot read: a failed read is a `warn`
    /// and not an abort. Ending a turn because a *read* failed would be the runner inventing a policy
    /// nobody configured; leaving the turn alone is the same choice the ladder makes about a backend call
    /// it could not answer.
    fn over_budget(&self, session_id: &str) -> Option<u64> {
        let ceiling = self.budget?;
        let spend = {
            let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
            match database.sessions().spend(session_id) {
                Ok(spend) => spend?,
                Err(error) => {
                    tracing::warn!(
                        target: "holosmith::turn",
                        session = %session_id,
                        %error,
                        "the session's spend could not be read; the budget waits for the next poll"
                    );
                    return None;
                }
            }
        };
        (spend >= ceiling).then_some(spend)
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
                //
                // The budget gets one last look here, and only here. A turn whose final frame is its
                // `message.end` can settle between two polls, and then there is nothing left to abort —
                // but the *session* is over its ceiling all the same, so the row is where the operator
                // reads why, and the next turn trips on its first poll before it does any work. It is
                // its own shape: a turn that finished after its deadline records `turn_timeout`
                // even though it stopped by itself. A turn that ended some *other* way keeps that
                // ending's reason: an operator's abort is not the budget's to relabel.
                // Every `agent_settled` leaves a WIP ref before the row moves (spec item 9). A snapshot
                // failure is logged and does not change the turn's ending: the turn did finish, and making a
                // failed git command turn a completed turn into an `error` row would lose the transcript it
                // just produced. the delete is where a snapshot failure is a refusal.
                self.snapshot(session).await;
                let reason = match reason {
                    Some(reason) => Some(reason),
                    None => self.settled_over_budget(&session.id).await,
                };
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

    /// Take the session's WIP snapshot, reporting a failure without changing the turn's ending.
    async fn snapshot(&self, session: &Session) {
        if let Err(error) = self.backend.snapshot(session).await {
            tracing::error!(
                target: "holosmith::turn",
                session = %session.id,
                %error,
                "the turn settled but its WIP snapshot failed; the worktree still holds the work"
            );
        }
    }

    /// The abort ending: `running -> idle` on `reason`, unless the ladder got there first.
    ///
    /// Two tasks can end one turn (ADR-0020): the ladder's second rung exists exactly for a
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
    /// which is what the LRU order and the idle stop read.
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
