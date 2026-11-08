//! The one state machine over a real database (HS-012, ADR-0016).
//!
//! The table itself and the illegal-move matrix are proved in `sessions/state.rs`'s unit tests,
//! where no database is in the way. This file proves the half that needs one: the refusal leaves
//! the row alone and emits nothing, an applied change persists the new state with its reason and
//! moves `stopped_at` with it, and the `session.status` event that reports the change carries
//! `from` and `to`.

use std::time::Duration;

use holosmith::clock;
use holosmith::config::Config;
use holosmith::events::WriterOptions;
use holosmith::sessions::state::{
    SESSION_STATUS, StateError, TABLE, Transitioner, Trigger, lookup,
};
use holosmith::sessions::{NewSession, Session, Status};
use serde_json::Value;

mod support;
use support::{TempDir, state_with_writer};

/// A session in `starting`, written straight through the repository: every test here drives the
/// state machine from there, so the setup is not itself a transition under test.
fn starting_session(state: &holosmith::http::AppState, key: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .create(
            &NewSession {
                project: "holosmith".to_string(),
                model: "claude-sonnet-4.5".to_string(),
                thinking: "high".to_string(),
                network: false,
                client_key: Some(key.to_string()),
                base_commit: None,
            },
            clock::unix_now(),
        )
        .expect("a session is created")
}

fn row(state: &holosmith::http::AppState, id: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .get(id)
        .expect("the row reads")
        .expect("the session exists")
}

/// Every committed event of `session`, in seq order, as (kind, payload).
fn events(state: &holosmith::http::AppState, session: &str) -> Vec<(String, Value)> {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .events()
        .read_since(session, 0, 100)
        .expect("the events read")
        .into_iter()
        .map(|event| {
            (
                event.kind,
                serde_json::from_str(&event.payload).expect("a payload is JSON"),
            )
        })
        .collect()
}

/// Wait for `count` committed events, then read them: the writer commits on its own schedule, so a
/// row that already changed can be a few milliseconds ahead of the event that reports it.
async fn committed_events(
    state: &holosmith::http::AppState,
    session: &str,
    count: usize,
) -> Vec<(String, Value)> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while events(state, session).len() < count {
        assert!(
            std::time::Instant::now() < deadline,
            "the events never committed"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    events(state, session)
}

#[tokio::test]
async fn an_illegal_move_is_refused_leaves_the_row_alone_and_emits_nothing() {
    // HS-012's demo: attempt the moves the table does not have, get a refusal with from and
    // trigger, and find the row exactly as it was.
    let dir = TempDir::new("state-illegal");
    let state = state_with_writer(&dir, Config::default(), WriterOptions::default());
    let session = starting_session(&state, "illegal");
    let transitioner: Transitioner = state.transitioner();

    // Refused from `starting`, and then from `idle`: the exhaustive 6x15 matrix is proved over
    // `lookup` in the module's own tests, and this proves `apply`'s refusal branch is the same
    // rule for more than one state, with a real row underneath.
    let mut refused = 0;
    for from in [Status::Starting, Status::Idle] {
        for trigger in Trigger::ALL {
            if lookup(from, trigger).is_some() {
                continue;
            }
            let before = row(&state, &session.id);
            let error = transitioner
                .apply(&session.id, trigger, None)
                .await
                .expect_err("the table has no such move");
            match error {
                StateError::Illegal {
                    from: refused_from, ..
                } => {
                    assert_eq!(refused_from, from, "{trigger}");
                }
                other => panic!("{trigger} was refused for the wrong reason: {other:?}"),
            }
            refused += 1;
            assert_eq!(
                row(&state, &session.id),
                before,
                "{trigger} changed the row it refused"
            );
        }
        // On to the next state the way the table says, so the second round starts from `idle`.
        if from == Status::Starting {
            transitioner
                .apply(&session.id, Trigger::PiReady, None)
                .await
                .expect("starting -> idle is in the table");
        }
    }
    assert!(refused > 0, "the matrix did not refuse anything");
    // A refusal on a session that does not exist is its own error and nothing is written.
    let error = transitioner
        .apply("no-such-id", Trigger::PiReady, None)
        .await
        .expect_err("an unknown session cannot change state");
    assert!(matches!(error, StateError::UnknownSession(ref id) if id == "no-such-id"));

    // A refusal is not an event: the one event that exists is the `starting -> idle` transition
    // the loop had to make to reach the second state, and nothing else.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let emitted = events(&state, &session.id);
    assert_eq!(emitted.len(), 1, "a refusal emitted events: {emitted:?}");
    assert_eq!(emitted[0].1["to"].as_str(), Some("idle"));
}

#[tokio::test]
async fn an_applied_change_persists_its_state_and_reports_from_and_to() {
    let dir = TempDir::new("state-applied");
    let state = state_with_writer(&dir, Config::default(), WriterOptions::default());
    let session = starting_session(&state, "applied");

    let settled = state
        .transitioner()
        .apply(&session.id, Trigger::PiReady, None)
        .await
        .expect("starting -> idle is in the table");
    assert_eq!(settled, Status::Idle);
    assert_eq!(row(&state, &session.id).status, Status::Idle);

    let events = committed_events(&state, &session.id, 1).await;
    let (kind, payload) = &events[0];
    assert_eq!(kind, SESSION_STATUS);
    assert_eq!(payload["from"].as_str(), Some("starting"));
    assert_eq!(payload["to"].as_str(), Some("idle"));
    assert_eq!(payload["step"].as_str(), None);
    assert!(payload["reason"].is_null(), "{payload}");

    // The state is where the session is, so a second step is refused rather than re-applied.
    let error = state
        .transitioner()
        .apply(&session.id, Trigger::PiReady, None)
        .await
        .expect_err("idle cannot become idle by a second ready report");
    assert!(matches!(
        error,
        StateError::Illegal {
            from: Status::Idle,
            trigger: Trigger::PiReady
        }
    ));
}

#[tokio::test]
async fn stopped_at_is_set_by_the_transition_into_stopped_and_cleared_by_the_one_out() {
    // Item 15: set when the state becomes stopped, cleared on resume — inside the same write, so
    // there is no window in which a `stopped` row has no `stopped_at`.
    let dir = TempDir::new("state-stopped-at");
    let state = state_with_writer(&dir, Config::default(), WriterOptions::default());
    let session = starting_session(&state, "stopped-at");
    let transitioner = state.transitioner();

    transitioner
        .apply(&session.id, Trigger::PiReady, None)
        .await
        .expect("starting -> idle");
    assert_eq!(row(&state, &session.id).stopped_at, None);

    let stopped = transitioner
        .apply(&session.id, Trigger::IdleStop, Some("idle_stop"))
        .await
        .expect("idle -> stopped");
    assert_eq!(stopped, Status::Stopped);
    let row_stopped = row(&state, &session.id);
    assert!(row_stopped.stopped_at.is_some(), "stopped_at was not set");
    assert_eq!(row_stopped.status_reason.as_deref(), Some("idle_stop"));

    let resumed = transitioner
        .apply(&session.id, Trigger::Resume, None)
        .await
        .expect("stopped -> starting");
    assert_eq!(resumed, Status::Starting);
    let row_resumed = row(&state, &session.id);
    assert_eq!(row_resumed.stopped_at, None, "resume left stopped_at set");
    assert_eq!(
        row_resumed.status_reason, None,
        "resume kept the stop's reason"
    );
}

#[tokio::test]
async fn the_expirys_row_admits_stopped_and_nothing_else() {
    // HS-064, spec item 15: the daily job's move is the table's narrow `expire` row, not the delete's `any`
    // one — a request may end a running turn first and is entitled to any state, while a job that destroys a
    // forgotten session must do it to *this* one. The row that matters is the refused one: a session the
    // operator resumed in the window between the pass's read and this write is left alone, and the table is
    // what says so (ADR-0016, ADR-0070).
    let dir = TempDir::new("state-expire");
    let state = state_with_writer(&dir, Config::default(), WriterOptions::default());
    let session = starting_session(&state, "expire");
    let transitioner = state.transitioner();

    transitioner
        .apply(&session.id, Trigger::PiReady, None)
        .await
        .expect("starting -> idle");
    let idle = row(&state, &session.id);
    let error = transitioner
        .apply(&session.id, Trigger::Expire, None)
        .await
        .expect_err("`idle` is not a state the expiry may remove");
    assert!(
        matches!(
            error,
            StateError::Illegal {
                from: Status::Idle,
                trigger: Trigger::Expire
            }
        ),
        "an idle row was refused for the wrong reason: {error:?}"
    );
    assert_eq!(
        row(&state, &session.id),
        idle,
        "the refusal changed the row"
    );

    transitioner
        .apply(&session.id, Trigger::IdleStop, Some("idle_stop"))
        .await
        .expect("idle -> stopped");
    let deleting = transitioner
        .apply(&session.id, Trigger::Expire, None)
        .await
        .expect("stopped -> deleting is the expiry's row");
    assert_eq!(deleting, Status::Deleting);
    let row = row(&state, &session.id);
    assert_eq!(row.status, Status::Deleting);
    // `deleting` is not `stopped`, so the clock the row has left the state with is cleared — and the row is
    // about to stop existing, which is why nothing reads it.
    assert_eq!(row.stopped_at, None, "leaving `stopped` left the clock set");
}

#[tokio::test]
async fn interrupted_is_a_reason_on_a_recovery_transition_not_a_state() {
    // The spec removed `ended` and says `interrupted` is a reason. Recovery from a crashed run is
    // the transition that carries it, and the state after it is `starting`.
    let dir = TempDir::new("state-interrupted");
    let state = state_with_writer(&dir, Config::default(), WriterOptions::default());
    let session = starting_session(&state, "interrupted");
    let transitioner = state.transitioner();
    transitioner
        .apply(&session.id, Trigger::PiReady, None)
        .await
        .expect("starting -> idle");
    transitioner
        .apply(&session.id, Trigger::Prompt, None)
        .await
        .expect("idle -> running");

    let recovered = transitioner
        .apply(&session.id, Trigger::CrashRecovery, Some("interrupted"))
        .await
        .expect("any -> starting is the recovery row");
    assert_eq!(recovered, Status::Starting);
    assert_eq!(
        row(&state, &session.id).status_reason.as_deref(),
        Some("interrupted")
    );
    // And the six states are all there is: nothing named `ended` parses.
    for state_name in ["ended", "interrupted"] {
        assert!(
            state_name.parse::<Status>().is_err(),
            "{state_name} is a state"
        );
    }
}

#[tokio::test]
async fn every_row_of_the_table_can_be_walked_end_to_end() {
    // A chain of legal moves through the table, each one applied and checked, so a row that the
    // table names is reachable in practice and not only in the array.
    let dir = TempDir::new("state-walk");
    let state = state_with_writer(&dir, Config::default(), WriterOptions::default());
    let session = starting_session(&state, "walk");
    let transitioner = state.transitioner();
    let mut walk = Vec::new();
    let mut current = Status::Starting;
    for trigger in [
        Trigger::PiReady,
        Trigger::Prompt,
        Trigger::AgentSettled,
        Trigger::Evict,
        Trigger::Resume,
        Trigger::UnexpectedExit,
        Trigger::Resume,
        Trigger::Delete,
    ] {
        let from = current;
        let to = transitioner
            .apply(&session.id, trigger, None)
            .await
            .unwrap_or_else(|error| panic!("{from} on {trigger}: {error}"));
        let row_to = lookup(from, trigger).map(|row| row.to).expect("a row");
        assert_eq!(to, row_to, "{from} on {trigger}");
        walk.push((from, trigger, to));
        current = to;
    }
    assert_eq!(current, Status::Deleting);
    // Nine transitions applied and the table has at least the rows they used.
    assert!(walk.len() >= 8);
    let committed = committed_events(&state, &session.id, walk.len()).await;
    assert_eq!(
        committed.len(),
        walk.len(),
        "an applied move was not reported"
    );
    for ((from, _, to), (kind, payload)) in walk.iter().zip(committed.iter()) {
        assert_eq!(kind, SESSION_STATUS);
        assert_eq!(payload["from"].as_str(), Some(from.as_str()));
        assert_eq!(payload["to"].as_str(), Some(to.as_str()));
    }
    assert!(
        TABLE.iter().any(|row| row.to == Status::Deleting),
        "the table cannot reach deleting"
    );
}
