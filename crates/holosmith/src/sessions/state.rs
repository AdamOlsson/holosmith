//! The one state machine: the spec's transition table as data, and the one function every state
//! change goes through (HS-012, ADR-0016).
//!
//! Three things live here, and they are all the same rule seen from three sides:
//!
//! - [`TABLE`] is the spec's table (item 8), one row per `(from, trigger)`. Nothing else in the
//!   crate decides which states may follow which.
//! - [`Transitioner::apply`] is the only production caller of the repository's status write. It
//!   looks the trigger up, refuses an illegal move with the from, the trigger and the to that was
//!   asked for — and **never** writes it — then persists the new state and emits the
//!   `session.status` event that reports the transition.
//! - [`SessionStatus`] is that event's payload: `from`, `to`, an optional `step` (the provisioning
//!   progress HS-011 reports) and an optional `reason`.
//!
//! `stopped_at` moves with the state inside the same write: set when the state becomes `stopped`,
//! cleared when it leaves, which is item 15's "cleared on resume" and the reason this is one
//! function rather than a helper beside each caller.
//!
//! The state `ended` does not exist: the six states are the spec's six, and "interrupted" is a
//! *reason* on a `session.status` event (see [`Trigger::CrashRecovery`]), never a state.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;

use crate::backend::Step;
use crate::clock;
use crate::db::Database;
use crate::events::{AppendError, EventWriter, NewEvent};
use crate::sessions::{RepoError, Status};

/// The event type every state change and every provisioning step is reported as.
pub const SESSION_STATUS: &str = "session.status";

/// What caused a state change: one trigger per row of the spec's table.
///
/// The names are the table's own words, lowercased in the event only if a later story needs them
/// there; today a trigger appears in a log line and in a refusal's message, which is why the type
/// derives `Display` through [`Self::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Trigger {
    /// A create was accepted for a row that does not exist yet.
    CreateAccepted,
    /// The provisioner finished all four steps and the agent is ready for a turn.
    PiReady,
    /// A provisioning step failed.
    ProvisioningFailed,
    /// A turn was asked to start.
    Prompt,
    /// The agent finished everything for the current turn.
    AgentSettled,
    /// The running turn ended because an abort was asked for and completed.
    AbortCompleted,
    /// The abort ladder (or the turn timeout) had to kill the container.
    AbortLadder,
    /// The cap needed a slot.
    Evict,
    /// The session sat idle too long.
    IdleStop,
    /// The machine is draining for a restart.
    Drain,
    /// A stopped or failed session is being brought back.
    Resume,
    /// The container exited on its own.
    UnexpectedExit,
    /// The container exited cleanly without having been asked to (spec item 6): code 0 and no
    /// expected-stop flag. The session ends `stopped` — the agent finished with no turn running — rather
    /// than `error`, which is what [`Self::UnexpectedExit`] is for.
    PiExit,
    /// A delete was asked for.
    Delete,
    /// The process restarted and this row was live when it died.
    CrashRecovery,
    /// The session's worktree is over `session_disk_soft_gb` (HS-063, spec item 14).
    ///
    /// An `idle` session's stop, and only that: a `running` one ends its turn through the abort ladder's own
    /// row ([`Self::AbortLadder`], with this policy's reason), because a turn is the ladder's to end and a row
    /// with no turn is the table's. The word is the reason the row records and the code the event carries —
    /// [`crate::quota::DISK_QUOTA`], and a test keeps the two spellings one.
    DiskQuota,
}

impl Trigger {
    /// Every trigger, so a test can prove the table and this enum agree.
    pub const ALL: [Trigger; 16] = [
        Trigger::CreateAccepted,
        Trigger::PiReady,
        Trigger::ProvisioningFailed,
        Trigger::Prompt,
        Trigger::AgentSettled,
        Trigger::AbortCompleted,
        Trigger::AbortLadder,
        Trigger::Evict,
        Trigger::IdleStop,
        Trigger::Drain,
        Trigger::Resume,
        Trigger::UnexpectedExit,
        Trigger::PiExit,
        Trigger::Delete,
        Trigger::CrashRecovery,
        Trigger::DiskQuota,
    ];

    /// The table's word for this trigger.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CreateAccepted => "create_accepted",
            Self::PiReady => "pi_ready",
            Self::ProvisioningFailed => "provisioning_failed",
            Self::Prompt => "prompt",
            Self::AgentSettled => "agent_settled",
            Self::AbortCompleted => "abort_completed",
            Self::AbortLadder => "abort_ladder",
            Self::Evict => "evict",
            Self::IdleStop => "idle_stop",
            Self::Drain => "drain",
            Self::Resume => "resume",
            Self::UnexpectedExit => "unexpected_exit",
            Self::PiExit => "pi_exit",
            Self::Delete => "delete",
            Self::CrashRecovery => "crash_recovery",
            Self::DiskQuota => "disk_quota",
        }
    }
}

impl fmt::Display for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which states a trigger may start from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FromStates {
    /// No row exists yet: the one transition that creates it.
    None,
    /// Exactly these states; any of them may take the trigger.
    OneOf(&'static [Status]),
    /// Every state.
    Any,
}

impl FromStates {
    /// Whether a row in `status` may take a trigger with this `from`.
    fn admits(self, status: Status) -> bool {
        match self {
            Self::None => false,
            Self::OneOf(statuses) => statuses.contains(&status),
            Self::Any => true,
        }
    }
}

/// One row of the spec's table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: FromStates,
    pub trigger: Trigger,
    pub to: Status,
}

/// The spec's transition table (item 8), in its order, as the one place a legal move is decided.
///
/// Read it against the spec's table row by row. Two rows need saying out loud:
///
/// - `running` + [`Trigger::AbortLadder`] ends in `stopped`; the spec's cell continues "then
///   immediate resume", which is a *second* row ([`Trigger::Resume`]) that HS-022 chains after
///   this one. One row is one state change.
/// - `any` + [`Trigger::Delete`] reaches `deleting`; "then removed" is the row's removal, which is
///   HS-019's flow and not a state.
/// - Recovery's `from` is the spec's "server crash", which is not a state: the rows that were
///   **live** when the process died are the ones with something to recover, so the row admits
///   `starting`, `idle` and `running`. A `stopped` session has nothing running and a `deleted` one
///   is gone; a row caught mid-delete is HS-044's decision, and the table refuses it until then
///   rather than restarting a session somebody asked to delete.
pub const TABLE: &[Transition] = &[
    Transition {
        from: FromStates::None,
        trigger: Trigger::CreateAccepted,
        to: Status::Starting,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Starting]),
        trigger: Trigger::PiReady,
        to: Status::Idle,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Starting]),
        trigger: Trigger::ProvisioningFailed,
        to: Status::Error,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Idle]),
        trigger: Trigger::Prompt,
        to: Status::Running,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Running]),
        trigger: Trigger::AgentSettled,
        to: Status::Idle,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Running]),
        trigger: Trigger::AbortCompleted,
        to: Status::Idle,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Running]),
        trigger: Trigger::AbortLadder,
        to: Status::Stopped,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Idle]),
        trigger: Trigger::Evict,
        to: Status::Stopped,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Idle]),
        trigger: Trigger::IdleStop,
        to: Status::Stopped,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Idle]),
        trigger: Trigger::Drain,
        to: Status::Stopped,
    },
    // Item 14's per-session soft cap (HS-063): an idle session whose worktree is over `session_disk_soft_gb`
    // is stopped and stays stopped until the operator cleans up, so this is a row of its own rather than a
    // second reason on `idle_stop` — the two answer different questions ("nobody used it" and "it is holding
    // too much disk"), and a client reads the reason. A `running` session does not take this row: its turn is
    // the abort ladder's to end, and the ladder writes `stopped` with the same reason (ADR-0069).
    Transition {
        from: FromStates::OneOf(&[Status::Idle]),
        trigger: Trigger::DiskQuota,
        to: Status::Stopped,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Stopped]),
        trigger: Trigger::Resume,
        to: Status::Starting,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Starting, Status::Idle, Status::Running]),
        trigger: Trigger::UnexpectedExit,
        to: Status::Error,
    },
    // Item 6's clean exit (HS-030): code 0 without the expected-stop flag ends the session *stopped*,
    // with `pi_exit` as its reason, rather than `error`. Item 8's table has no row for it — its
    // "container exits unexpectedly" cell is the one this rule replaces for a clean exit — so the row
    // is this story's amendment, recorded in ADR-0038. It is also the reason word, the way
    // `idle_stop`'s trigger is its reason's.
    Transition {
        from: FromStates::OneOf(&[Status::Starting, Status::Idle, Status::Running]),
        trigger: Trigger::PiExit,
        to: Status::Stopped,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Error]),
        trigger: Trigger::Resume,
        to: Status::Starting,
    },
    Transition {
        from: FromStates::Any,
        trigger: Trigger::Delete,
        to: Status::Deleting,
    },
    Transition {
        from: FromStates::OneOf(&[Status::Starting, Status::Idle, Status::Running]),
        trigger: Trigger::CrashRecovery,
        to: Status::Starting,
    },
];

/// The row `trigger` takes from `status`, if the move is legal.
pub fn lookup(status: Status, trigger: Trigger) -> Option<&'static Transition> {
    TABLE
        .iter()
        .find(|row| row.trigger == trigger && row.from.admits(status))
}

/// The state a newly created row holds: the table's `none` row, read rather than spelled again.
pub fn initial_status() -> Status {
    TABLE
        .iter()
        .find(|row| row.from == FromStates::None && row.trigger == Trigger::CreateAccepted)
        .map(|row| row.to)
        .expect("the table has exactly one row for a create")
}

/// Why a state change did not happen.
///
/// Not `Clone`/`PartialEq`: it carries a `RepoError`, which carries a `rusqlite::Error`, and a
/// test that needs to compare one matches on the variant instead (`matches!`).
#[derive(Debug)]
pub enum StateError {
    /// The move is not in the table. A bug at the caller, never a client error: the row is left
    /// exactly as it was.
    Illegal { from: Status, trigger: Trigger },
    /// No such session. The change is not applied and nothing is emitted.
    UnknownSession(String),
    /// The status write failed.
    Repository(RepoError),
    /// The state changed and the event that reports it did not reach the writer, which means the
    /// process is going down. The row is already right, so this is a warning, not a rollback.
    Unreported(AppendError),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Illegal { from, trigger } => {
                write!(f, "illegal transition: from {from} on {trigger}")
            }
            Self::UnknownSession(id) => write!(f, "there is no session \"{id}\""),
            Self::Repository(error) => write!(f, "{error}"),
            Self::Unreported(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for StateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            Self::Unreported(error) => Some(error),
            Self::Illegal { .. } | Self::UnknownSession(_) => None,
        }
    }
}

/// The payload of a `session.status` event.
///
/// Four keys, always present, `Option` fields explicit `null` — the shape rule the session row
/// follows, so a client decodes one thing. `step` is set only by a provisioning report (HS-011),
/// where `from` and `to` are both `starting` because a step beginning changes no state; a real
/// transition carries `step: null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionStatus {
    pub from: Status,
    pub to: Status,
    pub step: Option<Step>,
    pub reason: Option<String>,
}

impl SessionStatus {
    /// A transition's payload: no step, and the reason when the trigger has one.
    pub fn transitioned(from: Status, to: Status, reason: Option<String>) -> Self {
        Self {
            from,
            to,
            step: None,
            reason,
        }
    }

    /// A provisioning step starting: the state does not change, so `from` and `to` are the same.
    pub fn step(status: Status, step: Step) -> Self {
        Self {
            from: status,
            to: status,
            step: Some(step),
            reason: None,
        }
    }

    /// The payload as the `events.payload` column holds it.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("four string-ish fields always serialise")
    }
}

/// The one function every state change goes through.
///
/// It carries the two handles a change needs — the database it writes and the writer it reports
/// through — so a caller cannot have one without the other and forget the event.
#[derive(Debug, Clone)]
pub struct Transitioner {
    database: Arc<Mutex<Database>>,
    events: EventWriter,
}

impl Transitioner {
    pub fn new(database: Arc<Mutex<Database>>, events: EventWriter) -> Self {
        Self { database, events }
    }

    /// Apply `trigger` to `session_id`, persist the new state, and emit `session.status`.
    ///
    /// An illegal move is refused before anything is written, and logged with the from, the
    /// trigger and the state the caller asked for. It is a bug in the caller, so it is an `Err`
    /// and not a silent no-op: a story that invents a transition fails its own test instead of
    /// producing a row nobody can explain.
    pub async fn apply(
        &self,
        session_id: &str,
        trigger: Trigger,
        reason: Option<&str>,
    ) -> Result<Status, StateError> {
        let (from, to) = {
            // One synchronous block: the guard is not `Send`, so it cannot outlive this scope and
            // cannot be held across the `await` below (ADR-0012/ADR-0013).
            let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
            let repository = database.sessions();
            let current = repository
                .get(session_id)
                .map_err(StateError::Repository)?
                .ok_or_else(|| StateError::UnknownSession(session_id.to_string()))?;
            let Some(transition) = lookup(current.status, trigger) else {
                // `debug`, not `error`: since HS-015 a refused transition is the *expected* outcome
                // of a legal request (a prompt that lost a race to another prompt), and the callers
                // that treat it as impossible log it themselves.
                tracing::debug!(
                    target: "holosmith::sessions::state",
                    session = session_id,
                    from = current.status.as_str(),
                    trigger = trigger.as_str(),
                    "refused an illegal state change"
                );
                return Err(StateError::Illegal {
                    from: current.status,
                    trigger,
                });
            };
            let to = transition.to;
            let updated = repository
                .set_status(session_id, to, reason, clock::unix_now())
                .map_err(StateError::Repository)?
                .ok_or_else(|| StateError::UnknownSession(session_id.to_string()))?;
            (current.status, updated.status)
        };
        // After the write, never before: an event must not claim a state the row does not hold.
        let payload = SessionStatus::transitioned(from, to, reason.map(str::to_string));
        let event = NewEvent {
            session_id: session_id.to_string(),
            kind: SESSION_STATUS.to_string(),
            payload: payload.to_json(),
            created_at: clock::unix_now(),
        };
        if let Err(error) = self.events.append(event).await {
            tracing::error!(
                target: "holosmith::sessions::state",
                session = session_id,
                %error,
                "a session.status event was not queued"
            );
            return Err(StateError::Unreported(error));
        }
        tracing::info!(
            target: "holosmith::sessions::state",
            session = session_id,
            from = from.as_str(),
            to = to.as_str(),
            trigger = trigger.as_str(),
            "state changed"
        );
        Ok(to)
    }

    /// Apply one transition from a background runner: every failure is logged, never returned.
    ///
    /// A runner's state change reports work that already happened, so a refused transition (a bug)
    /// or a stopped writer must not make the task lie about the outcome. The provisioner and the
    /// turn runner differ only in what they do *around* this call, so the reporting lives here
    /// rather than once in each.
    pub async fn apply_logged(&self, session_id: &str, trigger: Trigger, reason: Option<&str>) {
        match self.apply(session_id, trigger, reason).await {
            Ok(_) => {}
            Err(error @ StateError::Illegal { .. }) => tracing::error!(
                target: "holosmith::sessions::state",
                session = session_id,
                %error,
                "a runner asked for a transition the table does not have"
            ),
            Err(error) => tracing::warn!(
                target: "holosmith::sessions::state",
                session = session_id,
                %error,
                "a runner's state change did not complete"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn every_trigger_has_a_word_and_every_word_is_the_tables() {
        assert_eq!(Trigger::ALL.len(), 16);
        for trigger in Trigger::ALL {
            assert_eq!(trigger.to_string(), trigger.as_str());
            let rows = TABLE.iter().filter(|row| row.trigger == trigger).count();
            assert!(
                rows > 0,
                "{trigger} is a trigger the table never uses, so nothing can ask for it"
            );
        }
        // `resume` is the one trigger with two rows: a stopped session and a failed one.
        assert_eq!(
            TABLE
                .iter()
                .filter(|row| row.trigger == Trigger::Resume)
                .count(),
            2
        );
    }

    #[test]
    fn the_table_is_the_specs_table_row_by_row() {
        // Each row read as `from -(trigger)-> to`, in the spec's order. Two spellings of the same
        // rule would be a second table; this test is what makes the table *the* table.
        let expected: Vec<(FromStates, Trigger, Status)> = vec![
            (FromStates::None, Trigger::CreateAccepted, Status::Starting),
            (
                FromStates::OneOf(&[Status::Starting]),
                Trigger::PiReady,
                Status::Idle,
            ),
            (
                FromStates::OneOf(&[Status::Starting]),
                Trigger::ProvisioningFailed,
                Status::Error,
            ),
            (
                FromStates::OneOf(&[Status::Idle]),
                Trigger::Prompt,
                Status::Running,
            ),
            (
                FromStates::OneOf(&[Status::Running]),
                Trigger::AgentSettled,
                Status::Idle,
            ),
            (
                FromStates::OneOf(&[Status::Running]),
                Trigger::AbortCompleted,
                Status::Idle,
            ),
            (
                FromStates::OneOf(&[Status::Running]),
                Trigger::AbortLadder,
                Status::Stopped,
            ),
            (
                FromStates::OneOf(&[Status::Idle]),
                Trigger::Evict,
                Status::Stopped,
            ),
            (
                FromStates::OneOf(&[Status::Idle]),
                Trigger::IdleStop,
                Status::Stopped,
            ),
            (
                FromStates::OneOf(&[Status::Idle]),
                Trigger::Drain,
                Status::Stopped,
            ),
            // Item 14's per-session soft cap (HS-063): the story's amendment, the way `pi_exit` is
            // ADR-0038's. A `running` session does not take this row — its turn is the abort ladder's.
            (
                FromStates::OneOf(&[Status::Idle]),
                Trigger::DiskQuota,
                Status::Stopped,
            ),
            (
                FromStates::OneOf(&[Status::Stopped]),
                Trigger::Resume,
                Status::Starting,
            ),
            (
                FromStates::OneOf(&[Status::Starting, Status::Idle, Status::Running]),
                Trigger::UnexpectedExit,
                Status::Error,
            ),
            (
                FromStates::OneOf(&[Status::Starting, Status::Idle, Status::Running]),
                Trigger::PiExit,
                Status::Stopped,
            ),
            (
                FromStates::OneOf(&[Status::Error]),
                Trigger::Resume,
                Status::Starting,
            ),
            (FromStates::Any, Trigger::Delete, Status::Deleting),
            (
                FromStates::OneOf(&[Status::Starting, Status::Idle, Status::Running]),
                Trigger::CrashRecovery,
                Status::Starting,
            ),
        ];
        assert_eq!(TABLE.len(), expected.len());
        for (row, (from, trigger, to)) in TABLE.iter().zip(expected) {
            assert_eq!((row.from, row.trigger, row.to), (from, trigger, to));
        }
    }

    #[test]
    fn a_create_starts_in_starting_and_the_table_is_where_that_is_read() {
        assert_eq!(initial_status(), Status::Starting);
        assert_eq!(lookup(Status::Starting, Trigger::CreateAccepted), None);
    }

    #[test]
    fn the_states_the_table_can_reach_are_the_specs_six_and_never_ended() {
        // `ended` was removed from the spec, and `interrupted` is a reason: no row reaches a state
        // outside the six, and no row's `to` is spelled as a string anywhere.
        for row in TABLE {
            assert!(
                Status::ALL.contains(&row.to),
                "a row reaches {}",
                row.to.as_str()
            );
        }
        for offered in ["ended", "interrupted", "removed"] {
            assert!(
                offered.parse::<Status>().is_err(),
                "{offered} parsed as a state"
            );
        }
    }

    #[test]
    fn an_illegal_move_is_refused_for_every_state_it_is_not_in() {
        // Every (state, trigger) pair the table does not name must be refused by `lookup`. This is
        // the matrix `apply` refuses, without a database in the way.
        for status in Status::ALL {
            for trigger in Trigger::ALL {
                let legal = lookup(status, trigger).is_some();
                let named = TABLE
                    .iter()
                    .any(|row| row.trigger == trigger && row.from.admits(status));
                assert_eq!(legal, named, "{status} on {trigger}");
            }
        }
        // Spot checks a reader will look for: `deleting` is a sink apart from `delete`, which
        // keeps it there. Recovery does not reach it — a row caught mid-delete is HS-044's
        // decision, and the table refuses rather than restarting a session somebody asked to
        // delete.
        for trigger in Trigger::ALL {
            let allowed = (trigger == Trigger::Delete).then_some(Status::Deleting);
            assert_eq!(
                lookup(Status::Deleting, trigger).map(|row| row.to),
                allowed,
                "a deleting session took {trigger}"
            );
        }
        assert_eq!(lookup(Status::Starting, Trigger::Prompt), None);
        for status in Status::ALL {
            assert_eq!(lookup(status, Trigger::CreateAccepted), None, "{status}");
        }
    }

    #[test]
    fn no_state_and_trigger_is_admitted_by_two_rows() {
        // `lookup` answers the first row that admits the pair, so overlapping rows would make the
        // table's order load-bearing. Today none overlap; this is what keeps a later `Any` row
        // from silently widening a move that sits above it.
        for status in Status::ALL {
            for trigger in Trigger::ALL {
                let admitted: Vec<Status> = TABLE
                    .iter()
                    .filter(|row| row.trigger == trigger && row.from.admits(status))
                    .map(|row| row.to)
                    .collect();
                assert!(
                    admitted.len() <= 1,
                    "{status} on {trigger} is admitted by {admitted:?}"
                );
            }
        }
    }

    #[test]
    fn an_illegal_move_says_from_the_trigger_and_nothing_else() {
        let error = StateError::Illegal {
            from: Status::Stopped,
            trigger: Trigger::Prompt,
        };
        assert_eq!(
            error.to_string(),
            "illegal transition: from stopped on prompt"
        );
    }

    #[test]
    fn the_payload_is_four_keys_in_order_with_explicit_nulls() {
        assert_eq!(
            SessionStatus::transitioned(Status::Starting, Status::Idle, None).to_json(),
            r#"{"from":"starting","to":"idle","step":null,"reason":null}"#
        );
        assert_eq!(
            SessionStatus::step(Status::Starting, Step::Network).to_json(),
            r#"{"from":"starting","to":"starting","step":"network","reason":null}"#
        );
        assert_eq!(
            SessionStatus::transitioned(
                Status::Starting,
                Status::Error,
                Some("the image is missing".to_string())
            )
            .to_json(),
            r#"{"from":"starting","to":"error","step":null,"reason":"the image is missing"}"#
        );
    }

    #[test]
    fn the_event_type_is_the_one_the_ui_and_the_story_name() {
        assert_eq!(SESSION_STATUS, "session.status");
    }

    /// The one file allowed to change a session's state outside the repository's definition.
    const THE_ONE_FUNCTION: &str = "sessions/state.rs";

    /// The file that defines the write.
    const REPOSITORY: &str = "sessions/repository.rs";

    /// Every file under `root` that names `set_status` outside the two allowed ones, as
    /// `<file>:<line>`.
    ///
    /// One function, used by the rule and by the planted test, so weakening the matcher cannot
    /// pass both. Note what the exemption is: the whole of `sessions/repository.rs`, because the
    /// `sessions` repository owns every statement against the table — including the `INSERT` whose
    /// `status` column the create writes from [`initial_status`].
    fn offenders_under(root: &Path) -> Vec<String> {
        let mut offenders = Vec::new();
        for (file, text) in crate::sql::guard::sources_under(root) {
            if file == THE_ONE_FUNCTION || file == REPOSITORY {
                continue;
            }
            for (line, text) in text.lines().enumerate() {
                if text.contains("set_status") {
                    offenders.push(format!("{file}:{}: set_status", line + 1));
                }
            }
        }
        offenders
    }

    #[test]
    fn no_status_write_outside_the_one_function() {
        // "Every state change calls one function; direct writes to `status` are impossible outside
        // it." `set_status` is the only statement that changes the column, so the rule is that the
        // crate's production code names it in exactly two places: its definition and this module.
        //
        // The walk covers `src/`, which is all the production code there is. A test outside the
        // crate may still call it — `tests/session_repository.rs` is HS-007's proof that the
        // repository can update a state at all — and that is the crate boundary doing its job
        // rather than an exemption here.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        // Non-vacuous: a walk that found nothing would pass the next assertion for the wrong
        // reason, so this one names the two files the rule is about (PER-39 S2's precedent).
        let files: Vec<String> = crate::sql::guard::sources_under(&root)
            .into_iter()
            .map(|(file, _)| file)
            .collect();
        for expected in [THE_ONE_FUNCTION, REPOSITORY] {
            assert!(
                files.iter().any(|file| file == expected),
                "the walk did not find src/{expected}"
            );
        }
        let offenders = offenders_under(&root);
        assert!(
            offenders.is_empty(),
            "a status write lives outside src/{THE_ONE_FUNCTION}: {offenders:?}"
        );
    }

    #[test]
    fn the_guard_fires_on_a_planted_write() {
        // The rule above passes over today's tree; this proves the same matcher would notice. The
        // planted directory is the guard module's own, so two tests cannot collide over one path.
        let planted = crate::sql::guard::TempDir::new("state-machine");
        planted.plant(
            "handler.rs",
            "let row = database.sessions().set_status(id, status, None, now)?;\n",
        );
        planted.plant("harmless.rs", "let status = Status::Idle;\n");
        planted.plant(REPOSITORY, "fn set_status(&self) {}\n");
        assert_eq!(
            offenders_under(planted.path()),
            ["handler.rs:1: set_status"]
        );
    }
}
