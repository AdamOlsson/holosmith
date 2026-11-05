//! The token budget (HS-023): a session's spend is persisted, and a ceiling ends the turn that spends it.
//!
//! The tally is the *writer's*, so these tests read the row: every `message.end` the stub emits adds its
//! counts to `usage_in` / `usage_out` inside the transaction that assigns the event's seq (ADR-0013's
//! funnel, ADR-0027).
//!
//! The ceiling is the *runner's*, so these tests run the runner instead of a server: `Turn::run` is
//! public for exactly that reason, as `Deleter::remove` and `Sweeper::sweep` are. `max_session_tokens` is
//! a count and not a duration, so unlike the timeout the tests can use a realistic value — the stub's
//! turn reports 1540 tokens, and a ceiling of 1000 is one prompt away. What the tests choose is the
//! *pace*: a stub whose frames take long enough for a poll to fall between the message's end and the
//! turn's.
//!
//! `running` is driven through the one state function, because the prompt endpoint would also spawn a
//! runner and the test would then be racing its own turn.

use std::sync::Arc;
use std::time::Duration;

use holosmith::backend::{Backends, Call, StubBackend};
use holosmith::clock;
use holosmith::config::Config;
use holosmith::http::AppState;
use holosmith::sessions::Session;
use holosmith::sessions::Status;
use holosmith::sessions::state::{Transitioner, Trigger};
use holosmith::turn::{BUDGET_EXCEEDED, Turn};
use serde_json::Value;

mod support;
use support::{PATIENCE, TempDir, state_in, with_stub};

/// The ladder's grace: comfortably longer than a frame of the stubs below, so rung 1 (the agent's own
/// abort) is what ends the turn and rung 2's stop-and-resume never fires in a test that is about the
/// budget.
const GRACE: Duration = Duration::from_millis(500);

/// What the stub's turn reports: 1200 in, 340 out.
const SPENT_IN: u64 = 1200;
const SPENT_OUT: u64 = 340;

/// A ceiling under one turn's 1540, so the first turn that reports usage is over it.
const CEILING: u64 = 1000;

/// A stub whose frames take `millis`, so there is room between a message's end and a turn's.
fn paced(millis: u64) -> StubBackend {
    StubBackend::new(Duration::from_millis(millis))
}

fn row(state: &AppState, id: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .get(id)
        .expect("the row reads")
        .expect("the session exists")
}

/// Every committed event of `session` as `(seq, kind, payload)`, in seq order.
fn events(state: &AppState, session: &str) -> Vec<(i64, String, Value)> {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .events()
        .read_since(session, 0, 400)
        .expect("the events read")
        .into_iter()
        .map(|event| {
            (
                event.seq,
                event.kind,
                serde_json::from_str(&event.payload).expect("a payload is JSON"),
            )
        })
        .collect()
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + PATIENCE;
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "{what} did not happen within {PATIENCE:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Wait for the row's tally to read `(input, output)`.
///
/// The tally is the event writer's, and the writer commits every 25 ms (ADR-0013), so a turn this test has
/// already awaited can still be one commit behind: the row is the fact, but not instantly. Waiting for the
/// value is the rule every story since HS-017 has had to relearn; sampling once is a coin toss — and this
/// file's first draft did exactly that and flaked in the gate.
async fn wait_for_usage(state: &AppState, id: &str, input: u64, output: u64) {
    wait_until("the tally is committed", || {
        let row = row(state, id);
        (row.usage_in, row.usage_out) == (input, output)
    })
    .await;
}

/// A session in `starting`, made the way the repository makes one, so a test can drive it to `idle`
/// and then to `running` without a server in the way.
fn session(state: &AppState, key: &str) -> Session {
    let now = clock::unix_now();
    let new = holosmith::sessions::NewSession {
        project: "holosmith".to_string(),
        model: "claude-sonnet-4.5".to_string(),
        thinking: "high".to_string(),
        base_commit: None,
        client_key: Some(key.to_string()),
        network: false,
    };
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .create(&new, now)
        .expect("the row inserts")
}

async fn drive(state: &AppState, id: &str, trigger: Trigger, reason: Option<&str>) {
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(id, trigger, reason)
        .await
        .expect("the move is in the table");
}

/// A session the caller has already moved to `running`: `starting -> idle -> running`.
async fn running(state: &AppState, key: &str) -> Session {
    let session = session(state, key);
    drive(state, &session.id, Trigger::PiReady, None).await;
    drive(state, &session.id, Trigger::Prompt, None).await;
    row(state, &session.id)
}

/// The runner a served process would build, with the two bounds a test chooses.
fn runner(
    state: &AppState,
    stub: &StubBackend,
    limit: Option<Duration>,
    budget: Option<u64>,
) -> Turn {
    Turn::new(
        Backends::Stub(stub.clone()),
        Arc::clone(&state.database),
        state.events.clone(),
        GRACE,
        limit,
        budget,
    )
}

#[tokio::test]
async fn a_turn_that_spends_the_budget_ends_with_the_reason() {
    // The session is already over the ceiling when this turn starts, which is what makes the trip
    // deterministic: the first poll falls a hundred milliseconds into a turn that lasts twelve frames.
    let dir = TempDir::new("budget-over");
    let stub = paced(200);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let spent = running(&state, "budget-over").await;

    // One turn with no ceiling: it settles, and the tally becomes the stub's 1540.
    runner(&state, &stub, None, None)
        .run(spent.clone(), "read the app shell".to_string())
        .await;
    wait_for_usage(&state, &spent.id, SPENT_IN, SPENT_OUT).await;
    let after_first = row(&state, &spent.id);
    assert_eq!(
        after_first.usage_in, SPENT_IN,
        "the tally was not persisted"
    );
    assert_eq!(
        after_first.usage_out, SPENT_OUT,
        "the tally was not persisted"
    );
    assert!(
        after_first.status_reason.is_none(),
        "a turn under no ceiling recorded a reason: {after_first:?}"
    );

    // The second turn starts over the ceiling and is ended before it does its work.
    drive(&state, &spent.id, Trigger::Prompt, None).await;
    let again = row(&state, &spent.id);
    runner(&state, &stub, None, Some(CEILING))
        .run(again, "and again".to_string())
        .await;

    let ended = row(&state, &spent.id);
    assert_eq!(ended.status, Status::Idle, "the session did not come back");
    assert_eq!(
        ended.status_reason.as_deref(),
        Some(BUDGET_EXCEEDED),
        "the row does not say why the turn ended"
    );
    // The ladder was asked, in the spec's order: rung 1 is the agent's own abort.
    let calls = stub.calls();
    assert!(
        calls.contains(&Call::ClearQueue) && calls.contains(&Call::Abort),
        "the ladder never asked: {calls:?}"
    );
    // One turn's worth of spend, not two: the aborted turn never reached its `message.end`.
    assert_eq!(
        (ended.usage_in, ended.usage_out),
        (SPENT_IN, SPENT_OUT),
        "the aborted turn was charged for a message it never sent"
    );
    // The reason reached the log, which is the transcript's copy of it.
    wait_until("the budget's transition is committed", || {
        events(&state, &spent.id).iter().any(|(_, kind, payload)| {
            kind == "session.status" && payload["reason"] == BUDGET_EXCEEDED
        })
    })
    .await;
}

#[tokio::test]
async fn a_turn_that_settles_over_its_ceiling_is_labelled_without_a_climb() {
    // The stub's turn lasts twelve frames of a millisecond, so the first poll (a hundred) falls after the
    // turn has already answered: there is nothing left to abort, and the row still has to say what
    // happened — HS-022's settle-after-the-deadline shape. The label is also the *only* thing this ending
    // can carry, so an absent ladder is what proves the path is the settle's and not the poll's.
    let dir = TempDir::new("budget-settled");
    let stub = paced(1);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "budget-settled").await;

    runner(&state, &stub, None, Some(CEILING))
        .run(session.clone(), "read the app shell".to_string())
        .await;

    let ended = row(&state, &session.id);
    assert_eq!(ended.status, Status::Idle, "the session did not come back");
    assert_eq!(
        ended.status_reason.as_deref(),
        Some(BUDGET_EXCEEDED),
        "a settled turn over its ceiling recorded no reason"
    );
    assert_eq!((ended.usage_in, ended.usage_out), (SPENT_IN, SPENT_OUT));
    let calls = stub.calls();
    assert!(
        !calls.contains(&Call::Abort) && !calls.contains(&Call::ClearQueue),
        "a turn that had already ended was aborted: {calls:?}"
    );
    wait_until("the settle's transition is committed", || {
        events(&state, &session.id)
            .iter()
            .any(|(_, kind, payload)| {
                kind == "session.status" && payload["reason"] == BUDGET_EXCEEDED
            })
    })
    .await;
}

#[tokio::test]
async fn two_turns_add_up() {
    // The tally is an **add**, not a set: a second turn reports its own counts, and the session's spend is
    // what both of them cost. A `SET` in `SessionRepository::add_usage` passes every other test in this
    // file — the ones that matter here are the two turns that both reach their `message.end`.
    let dir = TempDir::new("budget-two-turns");
    let stub = paced(1);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "budget-two-turns").await;

    runner(&state, &stub, None, None)
        .run(session.clone(), "read the app shell".to_string())
        .await;
    drive(&state, &session.id, Trigger::Prompt, None).await;
    let again = row(&state, &session.id);
    runner(&state, &stub, None, None)
        .run(again, "and again".to_string())
        .await;

    wait_for_usage(&state, &session.id, 2 * SPENT_IN, 2 * SPENT_OUT).await;
    let ended = row(&state, &session.id);
    assert_eq!(ended.status, Status::Idle, "the second turn did not settle");
    assert_eq!(
        (ended.usage_in, ended.usage_out),
        (2 * SPENT_IN, 2 * SPENT_OUT),
        "two turns that both reported usage did not add up"
    );
}

#[tokio::test]
async fn a_session_that_has_spent_exactly_its_ceiling_is_over_it() {
    // Spec item 13's "At `max_session_tokens`" is a boundary, not an overshoot: the stub's turn spends
    // 1540, so a ceiling of 1540 is reached by it, and a `>` in `Turn::over_budget` would leave the
    // session one token above where the operator drew the line for one more whole turn.
    let dir = TempDir::new("budget-at-the-limit");
    let stub = paced(1);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "budget-at-the-limit").await;

    runner(&state, &stub, None, Some(SPENT_IN + SPENT_OUT))
        .run(session.clone(), "read the app shell".to_string())
        .await;

    let ended = row(&state, &session.id);
    assert_eq!(ended.status, Status::Idle, "the session did not come back");
    assert_eq!(
        ended.status_reason.as_deref(),
        Some(BUDGET_EXCEEDED),
        "a session that spent exactly its ceiling was not over it"
    );
}

#[tokio::test]
async fn the_first_policy_to_fire_is_the_only_one_that_climbs() {
    // A turn that settles over its ceiling is the cheapest frame for this: the first turn spends, and
    // the second starts over the ceiling — so the budget fires at its first poll (100 ms) while the
    // deadline is still short of a turn that lasts twelve frames of 200 ms. The deadline is then one
    // the turn would have outlived anyway, so an unguarded second climb is a thing this test sees.
    let dir = TempDir::new("budget-one-climb");
    let stub = paced(300);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let spent = running(&state, "budget-one-climb").await;
    runner(&state, &stub, None, None)
        .run(spent.clone(), "read the app shell".to_string())
        .await;

    drive(&state, &spent.id, Trigger::Prompt, None).await;
    let again = row(&state, &spent.id);
    // A hundred milliseconds between the poll that fires and the deadline, and another hundred between
    // the deadline and the turn's first frame: the arm ordering cannot be decided by a scheduler stall on
    // either side, which is what a narrower window would leave to `select!`'s coin toss.
    runner(
        &state,
        &stub,
        Some(Duration::from_millis(200)),
        Some(CEILING),
    )
    .run(again, "and again".to_string())
    .await;

    let ended = row(&state, &spent.id);
    assert_eq!(ended.status, Status::Idle, "the session did not come back");
    assert_eq!(
        ended.status_reason.as_deref(),
        Some(BUDGET_EXCEEDED),
        "the first policy's reason was not the one recorded"
    );
    let calls = stub.calls();
    let aborts = calls.iter().filter(|call| **call == Call::Abort).count();
    assert_eq!(aborts, 1, "both policies climbed the ladder: {calls:?}");
}

#[tokio::test]
async fn an_unset_budget_never_ends_a_turn() {
    // The story's "unset means unlimited; the key is optional and absence is not an error".
    let dir = TempDir::new("budget-unset");
    let stub = paced(1);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "budget-unset").await;

    runner(&state, &stub, None, None)
        .run(session.clone(), "read the app shell".to_string())
        .await;

    wait_for_usage(&state, &session.id, SPENT_IN, SPENT_OUT).await;
    let ended = row(&state, &session.id);
    assert_eq!(
        ended.status,
        Status::Idle,
        "a turn under no ceiling ended early"
    );
    assert!(
        ended.status_reason.is_none(),
        "a turn under no ceiling recorded a reason: {ended:?}"
    );
    // The tally is still persisted: the ceiling's absence is not the tally's.
    assert_eq!((ended.usage_in, ended.usage_out), (SPENT_IN, SPENT_OUT));
}

#[tokio::test]
async fn a_backend_that_reports_no_usage_cannot_trip_the_budget() {
    // Spec item 13's contingency: the counts stay zero and a ceiling of 1 is never reached, so the turn
    // runs to its end. The screen's sentence is what tells the operator (HS-023's own rule,
    // `budgetNotice` in `web/src/transcript.ts`).
    let dir = TempDir::new("budget-no-usage");
    let stub = paced(1).omitting_usage();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "budget-no-usage").await;

    runner(&state, &stub, None, Some(1))
        .run(session.clone(), "read the app shell".to_string())
        .await;

    let ended = row(&state, &session.id);
    assert_eq!(
        ended.status,
        Status::Idle,
        "a turn with no usage ended early"
    );
    assert!(
        ended.status_reason.is_none(),
        "a backend that reports no usage tripped the budget: {ended:?}"
    );
    assert_eq!(
        (ended.usage_in, ended.usage_out),
        (0, 0),
        "the tally counted usage that was never reported"
    );
}

#[tokio::test]
async fn a_message_end_without_usage_adds_nothing_to_the_tally() {
    // The writer's rule, at its own seam: one turn that reports usage and one that does not, on one
    // session, leave the tally at the first turn's numbers — a missing `usage` is not a zero and not a
    // reset. The second stub is the one that omits; the state, and therefore the writer, is the first's.
    let dir = TempDir::new("budget-mixed");
    let stub = paced(1);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "budget-mixed").await;
    runner(&state, &stub, None, None)
        .run(session.clone(), "read the app shell".to_string())
        .await;

    let quiet = paced(1).omitting_usage();
    drive(&state, &session.id, Trigger::Prompt, None).await;
    let again = row(&state, &session.id);
    runner(&state, &quiet, None, None)
        .run(again, "and again".to_string())
        .await;

    wait_for_usage(&state, &session.id, SPENT_IN, SPENT_OUT).await;
    let ended = row(&state, &session.id);
    assert_eq!(ended.status, Status::Idle, "the second turn did not settle");
    assert_eq!(
        (ended.usage_in, ended.usage_out),
        (SPENT_IN, SPENT_OUT),
        "a `message.end` with no usage reset or doubled the tally"
    );
}

/// The tool fixture: two assistant messages of a real turn, whose reported usage is a real provider's.
const REAL_TURN: &str = include_str!("fixtures/pi-turn-tool.jsonl");

/// What that turn reported, in the contract's two counts (ADR-0042 folds the cache counters into `in`):
/// (1408 + 256) + (93 + 1664) prompt tokens and 76 + 5 generated ones.
const REAL_IN: u64 = 1664 + 1757;
const REAL_OUT: u64 = 76 + 5;

/// Queue the fixture's frames on `state`'s writer through the production mapping — no agent, no container.
///
/// This is the "real backend" half of HS-034: the numbers in the row come from a provider's own report as pi
/// printed it, mapped by `rpc::Mapping`, and nothing here writes a count by hand.
async fn replay_real_turn(state: &AppState, session: &Session) {
    let mut mapping = holosmith::rpc::Mapping::new(&session.id);
    for line in REAL_TURN.lines().filter(|line| !line.trim().is_empty()) {
        let record: Value = serde_json::from_str(line).expect("every fixture line is JSON");
        let Some((kind, payload)) = mapping.map(&record).into_frame() else {
            continue;
        };
        state
            .events
            .append(holosmith::events::NewEvent {
                session_id: session.id.clone(),
                kind: kind.to_string(),
                payload,
                created_at: clock::unix_now(),
            })
            .await
            .expect("the writer accepts the frame");
    }
}

#[tokio::test]
async fn a_real_turns_usage_becomes_the_sessions_tally_and_trips_the_ceiling() {
    // The acceptance's third bullet, honestly: a container cannot complete a turn before the key proxy exists
    // (HS-039), so "against the real backend" is a real turn's *records* through the production mapping and a
    // real runner over the row they land in. Every number below is the provider's; only the container is
    // HS-046's verification run.
    let dir = TempDir::new("budget-real");
    let stub = paced(200);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "budget-real").await;

    replay_real_turn(&state, &session).await;
    wait_for_usage(&state, &session.id, REAL_IN, REAL_OUT).await;

    // A ceiling under what the real turn cost, so the *next* turn is the one that is refused: the same shape
    // spec item 13 describes, with the numbers a provider reported rather than a stub's script.
    let ceiling = REAL_IN + REAL_OUT - 1;
    runner(&state, &stub, None, Some(ceiling))
        .run(row(&state, &session.id), "and again".to_string())
        .await;

    let ended = row(&state, &session.id);
    assert_eq!(ended.status, Status::Idle, "the session did not come back");
    assert_eq!(
        ended.status_reason.as_deref(),
        Some(BUDGET_EXCEEDED),
        "a turn that started over a ceiling the real turn cost did not end"
    );
    assert!(
        ended.usage_in >= REAL_IN && ended.usage_out >= REAL_OUT,
        "the tally lost the real turn's counts: {ended:?}"
    );
}

#[tokio::test]
async fn counts_survive_a_stop_and_a_resume() {
    // The tally is a column, and neither the ladder nor the provisioner touches it: a session that is stopped
    // and brought back has spent what it spent, which is what "per session" in spec item 13 means.
    let dir = TempDir::new("budget-resume");
    let stub = paced(1);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "budget-resume").await;

    runner(&state, &stub, None, None)
        .run(session.clone(), "read the app shell".to_string())
        .await;
    wait_for_usage(&state, &session.id, SPENT_IN, SPENT_OUT).await;

    // The row's own path through the table: `idle -> stopped` by the sweeper's trigger, then the resume's two
    // rows. No runner and no backend call, because the question is only what the column does.
    drive(&state, &session.id, Trigger::IdleStop, None).await;
    drive(&state, &session.id, Trigger::Resume, None).await;
    drive(&state, &session.id, Trigger::PiReady, None).await;

    let resumed = row(&state, &session.id);
    assert_eq!(resumed.status, Status::Idle, "the resume did not finish");
    assert_eq!(
        (resumed.usage_in, resumed.usage_out),
        (SPENT_IN, SPENT_OUT),
        "the stop and the resume changed what the session had spent"
    );
}
