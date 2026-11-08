//! The stopped-session expiry (HS-064, spec item 15, decision D3).
//!
//! Everything here runs without a daemon: the stub backend is the delete flow's other end, and it
//! *records* the calls it is given, which is how "the snapshot runs first" and "the flow ran, not a second
//! removal" become two assertions instead of two hopes. What a `TempDir` and a stub cannot carry — a real
//! repository's keep-rules, a real container's teardown — is `case_stopped_expiry`'s and `tests/worktree.rs`'s;
//! the point of this file is that the *policy* does the right thing with the rows, and that it hands the work to
//! the flow rather than re-implementing it.

use std::sync::Arc;
use std::time::Duration;

use holosmith::backend::{Call, StubBackend};
use holosmith::clock;
use holosmith::config::Config;
use holosmith::events::WriterOptions;
use holosmith::http::AppState;
use holosmith::sessions::state::Transitioner;
use holosmith::sessions::{NewSession, Session, Status};

mod support;
use support::{TempDir, state_with_writer};

/// A day, in the seconds every timestamp on this wire is in.
const DAY: i64 = 86_400;

/// One session created straight through the repository and moved to `status` at the instant `at`.
///
/// `set_status` is the raw write, which an integration test may make: this file's subject is the policy over
/// rows, not the transitioner's own legality, and `tests/state_machine.rs` owns that half. It is also the only
/// way to backdate the clock, because `set_status` writes `stopped_at = now` when the new state is `stopped` —
/// so passing a `now` of eight days ago *is* the story's stand-in for a seven-day wall clock, and it needs no
/// second statement (and no `SessionRepository` escape hatch).
fn session_at(state: &AppState, key: &str, status: Status, at: i64) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    let repository = database.sessions();
    let created = repository
        .create(
            &NewSession {
                project: "holosmith".to_string(),
                model: "claude-sonnet-4.5".to_string(),
                thinking: "high".to_string(),
                network: false,
                client_key: Some(key.to_string()),
                base_commit: None,
            },
            at,
        )
        .expect("a session is created");
    repository
        .set_status(&created.id, status, None, at)
        .expect("the row is written");
    repository
        .get(&created.id)
        .expect("the row reads back")
        .expect("the row is there")
}

/// How long a test waits for the flow's own removal task: the row goes off the request path, so "the row is
/// gone" is a fact that arrives shortly after the pass, not with it (HS-020's "a background call is not there
/// when the answer is").
const PATIENCE: Duration = Duration::from_secs(10);

/// Poll until `condition` holds, or fail naming what never happened.
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

/// Wait for a session's row to be gone, which is the delete flow's own last step.
async fn wait_gone(state: &AppState, id: &str) {
    wait_until("the session's row was removed", || {
        state
            .database
            .lock()
            .expect("the lock is not poisoned")
            .sessions()
            .get(id)
            .expect("the table is readable")
            .is_none()
    })
    .await;
}

/// A stub whose calls a test can read after the state has taken it.
fn stub() -> StubBackend {
    StubBackend::new(Duration::from_millis(1))
}

/// The state a test drives: its own database, its own directory, the stub it can still ask.
fn state(dir: &TempDir, stub: &StubBackend, config: Config) -> AppState {
    let state = state_with_writer(dir, config, WriterOptions::default());
    support::with_stub(state, stub.clone())
}

#[tokio::test]
async fn the_pass_removes_a_row_stopped_past_its_window_and_the_snapshot_ran_first() {
    // Acceptance bullets 2 and 3, in the success arm: a row stopped eight days ago on the default seven-day
    // window is gone after one pass, and the two calls the stub recorded are the delete flow's — the snapshot
    // *before* the removal, which is item 9's order and the reason a removal cannot lose work.
    let dir = TempDir::new("expiry-removes");
    let stub = stub();
    let state = state(&dir, &stub, Config::default());
    let now = clock::unix_now();
    let session = session_at(&state, "expiry-one", Status::Stopped, now - 8 * DAY);

    state.expiry().sweep().await;
    // The row is the flow's to remove and it runs in its own task, so the pass returning is the *promise*, not
    // the removal.
    wait_gone(&state, &session.id).await;

    let calls = stub.calls();
    let snapshot = calls
        .iter()
        .position(|call| *call == Call::Snapshot)
        .expect("the flow never took a snapshot");
    let delete = calls
        .iter()
        .position(|call| *call == Call::Delete)
        .expect("the flow never removed the session");
    assert!(
        snapshot < delete,
        "the removal ran before the snapshot: {calls:?}"
    );
    // The transition the flow wrote is the story's: `stopped -> deleting`, with no reason (the row is about to
    // stop existing, so nothing could read one).
    assert!(
        !calls.contains(&Call::Stop),
        "the expiry stopped a container instead of running the delete flow: {calls:?}"
    );
}

#[tokio::test]
async fn a_row_inside_its_window_is_left_alone() {
    // The window is a bound and not a suggestion: a row stopped a day ago on a seven-day window is a candidate
    // for nobody, and a pass that removed it would make `stopped_ttl_days` decorative.
    let dir = TempDir::new("expiry-window");
    let stub = stub();
    let state = state(&dir, &stub, Config::default());
    let now = clock::unix_now();
    let session = session_at(&state, "expiry-window", Status::Stopped, now - DAY);

    state.expiry().sweep().await;

    let database = state.database.lock().expect("the lock is not poisoned");
    assert!(
        database
            .sessions()
            .get(&session.id)
            .expect("the table is readable")
            .is_some(),
        "a session stopped a day ago was removed"
    );
    drop(database);
    assert!(
        stub.calls().is_empty(),
        "the flow ran for a row inside its window: {:?}",
        stub.calls()
    );
}

#[tokio::test]
async fn the_window_is_the_configs_and_a_long_one_keeps_an_eight_day_old_row() {
    // The number is `stopped_ttl_days`, not the spec's default baked into the policy: the same eight-day-old row
    // that goes on a seven-day window survives a thirty-day one.
    let dir = TempDir::new("expiry-ttl");
    let stub = stub();
    let config = Config {
        stopped_ttl_days: 30,
        ..Config::default()
    };
    let state = state(&dir, &stub, config);
    let now = clock::unix_now();
    let session = session_at(&state, "expiry-ttl", Status::Stopped, now - 8 * DAY);

    state.expiry().sweep().await;

    let database = state.database.lock().expect("the lock is not poisoned");
    assert!(
        database
            .sessions()
            .get(&session.id)
            .expect("the table is readable")
            .is_some(),
        "a thirty-day window removed an eight-day-old row"
    );
}

#[tokio::test]
async fn a_failed_snapshot_skips_the_row_and_leaves_it_a_candidate_for_tomorrow() {
    // Acceptance bullet 3's refusal arm, and the story's own contract: "if it fails, the session is skipped and
    // retried the next day". A policy must not force — `?force=true` discards work the operator has been told
    // about and a job has nobody to tell — so nothing about the row moves, which is *why* tomorrow's pass finds
    // it: the retry is the candidate set, not a second attempt inside this pass.
    let dir = TempDir::new("expiry-snapshot");
    let stub = stub().failing_snapshot();
    let state = state(&dir, &stub, Config::default());
    let now = clock::unix_now();
    let session = session_at(&state, "expiry-snapshot", Status::Stopped, now - 8 * DAY);

    state.expiry().sweep().await;

    let database = state.database.lock().expect("the lock is not poisoned");
    let after = database
        .sessions()
        .get(&session.id)
        .expect("the table is readable")
        .expect("the row was removed past a failed snapshot");
    assert_eq!(after.status, Status::Stopped, "the row left `stopped`");
    assert_eq!(
        after.stopped_at,
        Some(now - 8 * DAY),
        "the skip touched the clock, so the row's window restarted"
    );
    // And it is still the repository's candidate: that *is* "retried the next day".
    let candidates = database
        .sessions()
        .expired_before(now - 7 * DAY)
        .expect("the table is readable");
    assert!(
        candidates.iter().any(|row| row.id == session.id),
        "the skipped row is no longer a candidate, so nothing would retry it"
    );
    drop(database);

    let calls = stub.calls();
    assert!(
        calls.contains(&Call::Snapshot),
        "the snapshot was not even attempted: {calls:?}"
    );
    assert!(
        !calls.contains(&Call::Delete),
        "a failed snapshot was forced through to the removal: {calls:?}"
    );
}

#[tokio::test]
async fn an_error_row_is_never_auto_deleted() {
    // Acceptance bullet 4, and item 15's own sentence. The predicate is `status = 'stopped'`, so an `error` row
    // is not a candidate no matter how old its activity is — and it has no `stopped_at` at all, which is the
    // second reason the comparison cannot match it.
    let dir = TempDir::new("expiry-error");
    let stub = stub();
    let state = state(&dir, &stub, Config::default());
    let now = clock::unix_now();
    let session = session_at(&state, "expiry-error", Status::Error, now);

    state.expiry().sweep().await;

    let database = state.database.lock().expect("the lock is not poisoned");
    let after = database
        .sessions()
        .get(&session.id)
        .expect("the table is readable")
        .expect("an error row was auto-deleted");
    assert_eq!(after.status, Status::Error);
    assert_eq!(after.stopped_at, None, "an error row carries no stop clock");
    drop(database);
    assert!(
        stub.calls().is_empty(),
        "the flow ran for an error row: {:?}",
        stub.calls()
    );
}

#[tokio::test]
async fn every_reason_into_stopped_sets_the_clock_and_the_resume_clears_it() {
    // Acceptance bullet 1, as one witness for all six reasons the machine stops a session by itself:
    // `forced_abort` (the ladder), `evicted` (the cap), `idle_stop` (the sweeper), `server_restart` (the drain),
    // `pi_exit` (a clean exit) and `disk_quota` (the soft cap). Each drives the real `Transitioner::apply` from
    // its own `from` state, because the write that moves `stopped_at` is the transition's and not the policy's —
    // which is why no endpoint and no job can forget. Then `resume`, which is item 15's "cleared on resume".
    let dir = TempDir::new("expiry-stopped-at");
    let state = state_with_writer(&dir, Config::default(), WriterOptions::default());
    let now = clock::unix_now();
    let cases: [(&str, Status, &str); 6] = [
        ("abort_ladder", Status::Running, "forced_abort"),
        ("evict", Status::Idle, "evicted"),
        ("idle_stop", Status::Idle, "idle_stop"),
        ("drain", Status::Idle, "server_restart"),
        ("pi_exit", Status::Idle, "pi_exit"),
        ("disk_quota", Status::Idle, "disk_quota"),
    ];
    for (index, (trigger, from, reason)) in cases.into_iter().enumerate() {
        let parsed = match trigger {
            "abort_ladder" => holosmith::sessions::state::Trigger::AbortLadder,
            "evict" => holosmith::sessions::state::Trigger::Evict,
            "idle_stop" => holosmith::sessions::state::Trigger::IdleStop,
            "drain" => holosmith::sessions::state::Trigger::Drain,
            "pi_exit" => holosmith::sessions::state::Trigger::PiExit,
            _ => holosmith::sessions::state::Trigger::DiskQuota,
        };
        let id = {
            let database = state.database.lock().expect("the lock is not poisoned");
            let repository = database.sessions();
            let created = repository
                .create(
                    &NewSession {
                        project: "holosmith".to_string(),
                        model: "claude-sonnet-4.5".to_string(),
                        thinking: "high".to_string(),
                        network: false,
                        client_key: Some(format!("stopped-at-{index}")),
                        base_commit: None,
                    },
                    now,
                )
                .expect("a session is created");
            // The `from` state, written straight: this test's subject is the transition into `stopped`.
            repository
                .set_status(&created.id, from, None, now)
                .expect("the row is written");
            created.id
        };
        let transitioner: Transitioner = state.transitioner();
        let to = transitioner
            .apply(&id, parsed, Some(reason))
            .await
            .expect("the table has this row");
        assert_eq!(to, Status::Stopped, "{trigger}");
        let row = {
            let database = state.database.lock().expect("the lock is not poisoned");
            database
                .sessions()
                .get(&id)
                .expect("the table is readable")
                .expect("the row is there")
        };
        // A window, not an equality: `Transitioner::apply` reads its own `clock::unix_now()`, so a run that
        // crosses a second boundary would flake an exact assertion (HS-017's `last_active_at` lesson). The
        // claim is "the transition recorded when it happened", and this is that claim without the race.
        let stopped_at = row
            .stopped_at
            .unwrap_or_else(|| panic!("{trigger} reached `stopped` without recording when"));
        assert!(
            stopped_at >= now && stopped_at <= clock::unix_now(),
            "{trigger} recorded a clock outside the test's own window: {stopped_at} vs {now}"
        );
        assert_eq!(row.status_reason.as_deref(), Some(reason));

        // And out again: a resume is the only row leaving `stopped`, and it clears the clock in the same write.
        transitioner
            .apply(&id, holosmith::sessions::state::Trigger::Resume, None)
            .await
            .expect("stopped -> starting");
        let resumed = {
            let database = state.database.lock().expect("the lock is not poisoned");
            database
                .sessions()
                .get(&id)
                .expect("the table is readable")
                .expect("the row is there")
        };
        assert_eq!(
            resumed.stopped_at, None,
            "{trigger} left `stopped` with its clock still set"
        );
    }
}

#[tokio::test]
async fn a_row_that_left_stopped_is_not_a_candidate_at_all() {
    // Acceptance bullet 6's other half, and the cheaper one: a resumed row is not a candidate, because the
    // statement's predicate is `status = 'stopped'` and it answers before any policy code runs. A row that is
    // resumed *between* the pass's read and the flow's write is a different rule — the pass's status re-read and
    // `Trigger::Expire`'s `stopped`-only row — and it is isolated in `src/expiry.rs`'s own unit test, where the
    // interleaving can actually be built (`expire` is private, and a whole `sweep` cannot be interleaved).
    let dir = TempDir::new("expiry-refused");
    let stub = stub();
    let state = state(&dir, &stub, Config::default());
    let now = clock::unix_now();
    let session = session_at(&state, "expiry-refused", Status::Stopped, now - 8 * DAY);

    // The operator's resume, landing between the pass's read and the flow's move. `Expire` admits `stopped`
    // only, so the move is illegal — and the row is untouched.
    let transitioner = state.transitioner();
    transitioner
        .apply(
            &session.id,
            holosmith::sessions::state::Trigger::Resume,
            None,
        )
        .await
        .expect("stopped -> starting");

    state.expiry().sweep().await;

    let database = state.database.lock().expect("the lock is not poisoned");
    let after = database
        .sessions()
        .get(&session.id)
        .expect("the table is readable")
        .expect("a resumed session was removed by the expiry");
    assert_eq!(
        after.status,
        Status::Starting,
        "the expiry moved a row it did not own"
    );
    drop(database);
    assert!(
        !stub.calls().contains(&Call::Delete),
        "the expiry removed a resumed session: {:?}",
        stub.calls()
    );
}

#[tokio::test]
async fn a_pass_over_an_empty_machine_asks_the_flow_nothing() {
    // The cheapest negative: no candidates, no snapshot, no removal. It also proves the pass reaches the
    // repository at all — a statement that failed would be logged and the pass would end, which is what this
    // test would then have to notice in a second form.
    let dir = TempDir::new("expiry-empty");
    let stub = stub();
    let state = Arc::new(state(&dir, &stub, Config::default()));
    state.expiry().sweep().await;
    assert!(stub.calls().is_empty(), "{:?}", stub.calls());
    // The tick is the policy, so it is asserted where the policy is: `expiry::TICK` is one day and nothing else
    // in the build owns that number.
    assert_eq!(holosmith::expiry::TICK, Duration::from_secs(86_400));
    // And a second pass over the same empty machine is the same nothing, which is what "daily, forever" costs.
    state.expiry().sweep().await;
    assert!(stub.calls().is_empty(), "{:?}", stub.calls());
}

#[tokio::test]
async fn the_pass_removes_every_candidate_and_not_only_the_first() {
    // "All of them, not one", the repository's own doc: a pass that removed one row and left the rest for later
    // passes would make the window a function of how many sessions were forgotten. Two rows, one pass.
    let dir = TempDir::new("expiry-many");
    let stub = stub();
    let state = state(&dir, &stub, Config::default());
    let now = clock::unix_now();
    let first = session_at(&state, "expiry-many-1", Status::Stopped, now - 8 * DAY);
    let second = session_at(&state, "expiry-many-2", Status::Stopped, now - 30 * DAY);

    state.expiry().sweep().await;
    wait_gone(&state, &first.id).await;
    wait_gone(&state, &second.id).await;

    // Two removals, and neither is the other's: the two ids the flow was given are the two rows the pass chose.
    // Their *order* is the repository's `ORDER BY stopped_at` and is asserted where the statement lives
    // (`tests/session_repository.rs`), not here — the flow's two removal tasks run concurrently, so a race would
    // make this a coin toss and the ordering claim half-strength.
    let mut deletes = stub
        .recorded()
        .into_iter()
        .filter(|recorded| recorded.call == Call::Delete)
        .map(|recorded| recorded.session)
        .collect::<Vec<_>>();
    deletes.sort();
    let mut expected = vec![first.id.clone(), second.id.clone()];
    expected.sort();
    assert_eq!(deletes, expected);
}
