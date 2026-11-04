//! The turn timeout (HS-022): a turn that will not end is ended for it.
//!
//! The deadline is the turn runner's own, so these tests run the runner instead of a server: `Turn::run`
//! is public for exactly that reason, as `Deleter::remove` and `Sweeper::sweep` are. A minute is the
//! config's unit and no test waits one, so the runner is built directly with a deadline the test chooses
//! — the same lever `abort_grace` gives the ladder tests, and the only one that does not mean editing a
//! config file between runs.
//!
//! `running` is driven through the one state function, because the prompt endpoint would also spawn a
//! runner and the test would then be racing its own turn.

use std::sync::Arc;
use std::time::Duration;

use holosmith::backend::{Call, StubBackend};
use holosmith::clock;
use holosmith::config::Config;
use holosmith::http::AppState;
use holosmith::sessions::Session;
use holosmith::sessions::Status;
use holosmith::sessions::state::{Transitioner, Trigger};
use holosmith::turn::{TURN_TIMEOUT, Turn, timeout};
use serde_json::Value;

mod support;
use support::{PATIENCE, TempDir, fast_stub, state_in, with_stub};

/// The ladder's grace in these tests: short enough that a forced stop is a beat, long enough that the
/// stub's own abort is not mistaken for it.
const GRACE: Duration = Duration::from_millis(100);

/// The deadline under test, in the runner rather than in a config file.
const TIMEOUT: Duration = Duration::from_millis(100);

/// A stub whose turn lasts twelve frames of `millis`, so a deadline shorter than that has something to
/// interrupt. The pace is the only knob the stub has for "a turn that takes time".
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
        .read_since(session, 0, 200)
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

/// Wait for a committed event whose payload says `reason`, and answer how many there are.
///
/// The writer commits every 25 ms, so a row that already carries the outcome does not mean its event is
/// readable yet — the lesson every story since HS-017 has had to relearn.
async fn wait_for_reason(
    state: &AppState,
    id: &str,
    reason: &str,
    want: usize,
) -> Vec<(i64, String, Value)> {
    let reason = reason.to_string();
    wait_until("the timeout's transition is committed", || {
        events(state, id)
            .iter()
            .filter(|(_, _, payload)| payload["reason"] == reason.as_str())
            .count()
            >= want
    })
    .await;
    events(state, id)
}

/// A session in `starting`, made the way the repository makes one, so a test can drive it to `idle` and
/// then to `running` without a server in the way.
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

/// The runner a served process would build, with the deadline a test chooses and no token ceiling: this
/// file is about the clock, and `tests/token_budget.rs` owns the budget (HS-023).
fn runner(state: &AppState, stub: &StubBackend, limit: Option<Duration>) -> Turn {
    Turn::new(
        holosmith::backend::Backends::Stub(stub.clone()),
        Arc::clone(&state.database),
        state.events.clone(),
        GRACE,
        limit,
        None,
    )
}

#[tokio::test]
async fn a_turn_past_its_timeout_ends_with_the_reason() {
    let dir = TempDir::new("turn-timeout-fires");
    let stub = paced(50);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "timeout-fires").await;

    runner(&state, &stub, Some(TIMEOUT))
        .run(session.clone(), "read the app shell".to_string())
        .await;

    let ended = row(&state, &session.id);
    assert_eq!(ended.status, Status::Idle, "the session did not come back");
    assert_eq!(
        ended.status_reason.as_deref(),
        Some(TURN_TIMEOUT),
        "the row does not say why the turn ended"
    );
    // The ladder asked the agent to stop before it killed anything: rung 1, in the spec's order.
    let calls = stub.calls();
    assert!(
        calls.contains(&Call::ClearQueue) && calls.contains(&Call::Abort),
        "the ladder never asked: {calls:?}"
    );
    assert!(
        !calls.contains(&Call::Stop),
        "a turn that answered the abort was force-stopped: {calls:?}"
    );

    // The story's second bullet: the reason is in the transcript, on the transition that ended it.
    let stopped_at = wait_for_reason(&state, &session.id, TURN_TIMEOUT, 1)
        .await
        .into_iter()
        .find(|(_, _, payload)| payload["reason"] == TURN_TIMEOUT)
        .expect("the log carries the timeout's transition");
    assert_eq!(stopped_at.1, "session.status");
    assert_eq!(stopped_at.2["from"], "running");
    assert_eq!(stopped_at.2["to"], "idle");
}

#[tokio::test]
async fn a_turn_shorter_than_its_timeout_settles_and_says_nothing_about_a_timeout() {
    let dir = TempDir::new("turn-timeout-quiet");
    // A millisecond a frame: the turn is over long before the deadline, which is what most turns do.
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "timeout-quiet").await;

    runner(&state, &stub, Some(TIMEOUT))
        .run(session.clone(), "read the app shell".to_string())
        .await;

    let ended = row(&state, &session.id);
    assert_eq!(ended.status, Status::Idle);
    assert_eq!(
        ended.status_reason, None,
        "a settled turn recorded a reason it does not have"
    );
    let calls = stub.calls();
    assert!(
        !calls.contains(&Call::Abort) && !calls.contains(&Call::Stop),
        "the deadline fired on a turn that ended: {calls:?}"
    );
    wait_until("the settle's event is committed", || {
        events(&state, &session.id)
            .iter()
            .any(|(_, _, payload)| payload["from"] == "running")
    })
    .await;
    let (_, kind, payload) = events(&state, &session.id)
        .into_iter()
        .find(|(_, _, payload)| payload["from"] == "running")
        .expect("the settle's transition is in the log");
    assert_eq!(kind, "session.status");
    assert_eq!(
        payload["reason"],
        Value::Null,
        "an agent settle has no reason"
    );
}

#[tokio::test]
async fn each_turn_gets_its_own_clock() {
    // "Per turn, not per session": two turns of the same session each time out, because the deadline is
    // built by the runner rather than kept on the row. A session-wide clock would let the second turn
    // inherit the first one's spent time and end instantly — or never end at all.
    let dir = TempDir::new("turn-timeout-per-turn");
    let stub = paced(50);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "timeout-per-turn").await;

    runner(&state, &stub, Some(TIMEOUT))
        .run(session.clone(), "first".to_string())
        .await;
    assert_eq!(row(&state, &session.id).status, Status::Idle);

    // The second turn settles in about a dozen milliseconds, well inside the same deadline. If the clock
    // were the session's rather than the turn's, the time the first turn spent would already be on it and
    // this turn would be aborted the moment it started — so "it settles, with no reason, and quickly" is
    // the assertion that tells the two designs apart.
    drive(&state, &session.id, Trigger::Prompt, None).await;
    let second = row(&state, &session.id);
    let started = std::time::Instant::now();
    runner(&state, &fast_stub(), Some(TIMEOUT))
        .run(second, "second".to_string())
        .await;
    let elapsed = started.elapsed();

    let ended = row(&state, &session.id);
    assert_eq!(ended.status, Status::Idle);
    assert_eq!(
        ended.status_reason, None,
        "a fresh turn inherited the previous turn's spent time"
    );
    assert!(
        elapsed < TIMEOUT,
        "the second turn took {elapsed:?}, which is the first turn's clock"
    );
    let timeouts = wait_for_reason(&state, &session.id, TURN_TIMEOUT, 1)
        .await
        .into_iter()
        .filter(|(_, _, payload)| payload["reason"] == TURN_TIMEOUT)
        .count();
    assert_eq!(timeouts, 1, "a settled turn recorded a timeout");
}

#[tokio::test]
async fn an_agent_that_ignores_the_abort_is_force_stopped_with_the_reason() {
    // Rung 2: the story's reason has to be the ladder's when the agent never answers, which is what
    // `ignoring_abort` produces. The forced stop is followed by the ladder's own resume, so the row is
    // brought back — the reason survives in the transcript, which is where a client reads it.
    let dir = TempDir::new("turn-timeout-forced");
    let stub = paced(50).ignoring_abort();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "timeout-forced").await;

    runner(&state, &stub, Some(TIMEOUT))
        .run(session.clone(), "read the app shell".to_string())
        .await;

    let calls = stub.calls();
    assert!(
        calls.contains(&Call::Stop),
        "an agent that ignored the abort was never force-stopped: {calls:?}"
    );
    wait_until("the forced stop's event is committed", || {
        events(&state, &session.id)
            .iter()
            .any(|(_, _, payload)| payload["to"] == "stopped")
    })
    .await;
    let forced = events(&state, &session.id)
        .into_iter()
        .find(|(_, _, payload)| payload["to"] == "stopped")
        .expect("the forced stop is in the log");
    assert_eq!(forced.1, "session.status");
    assert_eq!(forced.2["from"], "running");
    assert_eq!(forced.2["reason"], TURN_TIMEOUT);

    // And the session is not left stopped. This is the assertion that catches a climb cut in half: the
    // ladder moves the row to `stopped` and then resumes it, so a ladder whose future was dropped between
    // those two steps leaves a session nobody will ever bring back (spec item 16 §2).
    wait_until("the ladder resumed the force-stopped session", || {
        row(&state, &session.id).status != Status::Stopped
    })
    .await;
}

#[tokio::test]
async fn no_timeout_configured_aborts_nothing() {
    // The off switch, observed rather than asserted on a field: with `None` the runner awaits the turn and
    // nothing else, so the ending is the agent's own and the ladder is never asked.
    let dir = TempDir::new("turn-timeout-off");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = running(&state, "timeout-off").await;

    runner(&state, &stub, None)
        .run(session.clone(), "read the app shell".to_string())
        .await;

    let ended = row(&state, &session.id);
    assert_eq!(ended.status, Status::Idle);
    assert_eq!(ended.status_reason, None);
    let calls = stub.calls();
    assert!(
        !calls.contains(&Call::Abort) && !calls.contains(&Call::Stop),
        "a disabled timeout aborted the turn: {calls:?}"
    );
}

#[tokio::test]
async fn zero_disables_the_timeout_and_the_default_does_not() {
    // `timeout` is the one place the key becomes a duration, so its two edges are proved here: 0 is the
    // off switch (no timer at all), and a key the operator set is the minutes they asked for.
    assert_eq!(timeout(0), None, "0 built a deadline");
    assert_eq!(timeout(1), Some(Duration::from_secs(60)));
    assert_eq!(Config::default().turn_timeout_min, 45, "the default moved");
    assert_eq!(
        timeout(Config::default().turn_timeout_min),
        Some(Duration::from_secs(45 * 60))
    );
    // An absurd number saturates rather than wrapping to a bound that fires at once.
    assert_eq!(timeout(u64::MAX), Some(Duration::from_secs(u64::MAX)));
}
