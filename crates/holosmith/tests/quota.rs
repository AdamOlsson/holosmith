//! The per-session disk soft cap (HS-063, spec item 14): a session that hoards disk is stopped, so one bad
//! build cannot starve the others.
//!
//! The policy is a task rather than an endpoint, so these tests call its one pass — `Quota::sweep` is public
//! for exactly that reason, as `Sweeper::sweep` and `Compactor::sweep` are — and assert the row, the backend
//! call and the event. Three things are the test's to own:
//!
//! - **The measurement.** `du` is a program, so every test that measures points `AppState.tools.du` at a shim
//!   it wrote: this sandbox has a real `du`, but a test cannot make a 10 GB tree, and a cap is a *number*
//!   rather than a size. The one test that uses the machine's own `du` is the one about what a worktree's
//!   size means (`target/` and `node_modules/` are inside it).
//! - **The worktree.** A stub session provisions nothing outside the process (ADR-0015), so its row has no
//!   worktree; the tests write the column the provisioner would have written (`set_worktree`, which is the
//!   repository's own method) and leave everything else to the product's path.
//! - **The state.** `idle` is produced the way the product produces it (a fast stub provisioning) and `running`
//!   through the one state function, because no endpoint puts a session in `running` without a turn behind it.
//!
//! The shim-writing tests take `one_at_a_time`, which is `tests/disk.rs`'s lock for the same reason: a shim is
//! written and then exec'd, and a *fork* inherits the open writing descriptor of a shim another thread wrote —
//! `exec` of a file still open for writing is `ETXTBSY`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use holosmith::backend::Call;
use holosmith::config::{Config, Duration as ConfigDuration};
use holosmith::http::{self, AppState};
use holosmith::quota::{self, DISK_QUOTA, Quota};
use holosmith::sessions::Session;
use holosmith::sessions::Status;
use holosmith::sessions::state::{Transitioner, Trigger};
use serde_json::Value;

mod support;
use support::{PATIENCE, Response, Served, TempDir, fast_stub, request, state_in, with_stub};

const GRACE: std::time::Duration = std::time::Duration::from_millis(100);

const ORIGIN: &str = "http://localhost:8080";

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

/// Hold this for the whole body of a test here: one shim-writing test at a time.
///
/// `exec` of a file that *any* process holds open for writing fails with `ETXTBSY`, and `Command`'s fork
/// happens on another thread's behalf, so a shim written a moment earlier can still be open when the `du` this
/// file planted is executed. `tests/disk.rs` measured it at roughly 3 runs in 40 before its lock; every test
/// here that measures writes a shim and then runs it, and nothing else in this binary forks, so serialising
/// each whole body is the whole fix.
async fn one_at_a_time() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

/// A config with a cap of `gb`, a grace this test can wait out and room for its own sessions.
fn capped(gb: u64) -> Config {
    Config {
        session_disk_soft_gb: gb,
        max_live_sessions: 8,
        // A forced stop waits this out before rung 2, so a test that measures the forced rung wants it short.
        abort_grace: ConfigDuration::from_millis(50),
        idle_stop_min: 0,
        turn_timeout_min: 0,
        ..Config::default()
    }
}

/// A writable program that answers `du -sk` with `kib` for every path, and appends the arguments it was
/// given to `log` so a test can read what the watchdog asked about.
fn du_shim(dir: &TempDir, kib: u64, log: &Path) -> PathBuf {
    let path = dir.file("du");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\nprintf '{kib}\\t/work\\n'\n",
            log = log.display()
        ),
    )
    .expect("the shim is writable");
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("the shim is executable");
    path
}

/// A `du` that refuses, for the arm where a measurement that cannot run must not read as a pass.
fn failing_shim(dir: &TempDir) -> PathBuf {
    let path = dir.file("du");
    std::fs::write(&path, "#!/bin/sh\necho 'du: cannot read it' >&2\nexit 1\n").expect("a shim");
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("the shim is executable");
    path
}

async fn post_create(addr: SocketAddr, key: &str) -> Response {
    let text = format!(
        "POST /api/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nIdempotency-Key: {key}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
        BODY.len()
    );
    request(addr, &text).await
}

/// A create that must be accepted, answering its id.
fn created_id(response: &Response) -> String {
    assert_eq!(response.status, 202, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("a create answers JSON");
    body["id"]
        .as_str()
        .expect("the response names an id")
        .to_string()
}

fn row(state: &AppState, id: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .get(id)
        .expect("the row reads")
        .expect("the session exists")
}

/// Record the worktree the provisioner would have recorded, so the policy has something to measure.
fn record_worktree(state: &AppState, id: &str, path: &Path) {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .set_worktree(id, &path.display().to_string(), "0000000000000000")
        .expect("the worktree column is writable");
}

/// The `session.status` frames of `session`, which is where a client reads the state from.
fn transitions(state: &AppState, id: &str) -> Vec<(String, Value)> {
    events(state, id)
        .into_iter()
        .filter(|(kind, _)| kind == "session.status")
        .collect()
}

/// Every committed event of `session` as `(kind, payload)`, in seq order.
fn events(state: &AppState, session: &str) -> Vec<(String, Value)> {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .events()
        .read_since(session, 0, 200)
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

/// The `error` frames this policy wrote for `session`.
fn reported(state: &AppState, session: &str) -> Vec<Value> {
    events(state, session)
        .into_iter()
        .filter(|(kind, _)| kind == "error")
        .map(|(_, payload)| payload)
        .collect()
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + PATIENCE;
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "{what} did not happen within {PATIENCE:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

async fn wait_status(state: &AppState, id: &str, want: Status) {
    wait_until("the session reached the expected state", || {
        row(state, id).status == want
    })
    .await;
}

/// One state change through the one state function, for the states no endpoint produces on its own.
async fn drive(state: &AppState, id: &str, trigger: Trigger, reason: Option<&str>) {
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(id, trigger, reason)
        .await
        .expect("the move is in the table");
}

/// The watchdog this state's config asks for: the wiring `serve` uses, not a second one.
fn quota(state: &AppState) -> Quota {
    state.quota().expect("this config keeps the cap")
}

/// What the backend was asked to do for `id`, without the provisioning a create always does.
fn lifecycle(stub: &holosmith::backend::StubBackend, id: &str) -> Vec<Call> {
    stub.recorded()
        .into_iter()
        .filter(|call| call.session == id)
        .map(|call| call.call)
        .filter(|call| !matches!(call, Call::Provision(_) | Call::Undo(_)))
        .collect()
}

/// A state with a cap of `gb` whose `du` is `program`, and a fast stub a test can inspect.
fn harness(
    dir: &TempDir,
    gb: u64,
    program: PathBuf,
) -> (AppState, holosmith::backend::StubBackend) {
    let stub = fast_stub();
    let mut state = with_stub(state_in(dir, capped(gb)), stub.clone());
    state.tools.du = program;
    (state, stub)
}

#[tokio::test]
async fn an_idle_session_over_the_cap_is_stopped_and_reported() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("quota-idle");
    let log = dir.file("du.log");
    let shim = du_shim(&dir, 15 * 1024 * 1024, &log);
    let (state, stub) = harness(&dir, 10, shim);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "quota-idle-1").await);
    wait_status(&state, &id, Status::Idle).await;
    let worktree = dir.path().join("worktrees").join(&id);
    std::fs::create_dir_all(&worktree).expect("a worktree path");
    record_worktree(&state, &id, &worktree);

    quota(&state).sweep().await;

    // The row: `stopped` with the cap's reason — the third acceptance bullet — and not `error`.
    let stopped = row(&state, &id);
    assert_eq!(stopped.status, Status::Stopped, "the cap did not stop it");
    assert_eq!(
        stopped.status_reason.as_deref(),
        Some(DISK_QUOTA),
        "the row does not say why it stopped"
    );
    assert!(stopped.stopped_at.is_some(), "`stopped_at` moves with it");
    assert_eq!(stopped.worktree, worktree.display().to_string());

    // The measurement: `du -sk -- <the session's own worktree>` and nothing else.
    let asked = std::fs::read_to_string(&log).expect("the shim logged its arguments");
    assert!(
        asked
            .trim()
            .ends_with(&format!("-- {}", worktree.display())),
        "the watchdog measured something else: {asked:?}"
    );

    // The container: `Stopper`'s call, so `expected_stop` was written first (the stub takes a snapshot on
    // its way out, which is the shape every stop has).
    wait_until("the stop reached the backend", || {
        lifecycle(&stub, &id).contains(&Call::Stop)
    })
    .await;

    // The event: one `error` frame, the code, the measured number and the cap it crossed.
    wait_until("the cap's event is committed", || {
        !reported(&state, &id).is_empty()
    })
    .await;
    let frames = reported(&state, &id);
    assert_eq!(frames.len(), 1, "one crossing is one event: {frames:?}");
    assert_eq!(frames[0]["code"], DISK_QUOTA);
    assert_eq!(frames[0]["size_kib"], 15 * 1024 * 1024);
    assert_eq!(frames[0]["limit_gb"], 10);
    let message = frames[0]["message"].as_str().expect("a sentence");
    assert!(message.contains("15.0 GB"), "{message}");
    assert!(message.contains("10 GB"), "{message}");

    // …and the transition a client reads the state from, with the same reason word as the code.
    let stop = transitions(&state, &id)
        .into_iter()
        .find(|(_, payload)| payload["reason"] == DISK_QUOTA)
        .expect("the log carries the stop");
    assert_eq!(stop.1["from"], "idle");
    assert_eq!(stop.1["to"], "stopped");
    assert_eq!(stop.1["step"], Value::Null);
}

#[tokio::test]
async fn a_session_under_the_cap_is_left_alone() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("quota-under");
    let log = dir.file("du.log");
    // The same worktree, the same shim, one number different: the comparison is what this test owns.
    let shim = du_shim(&dir, 10 * 1024 * 1024, &log);
    let (state, stub) = harness(&dir, 10, shim);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "quota-under-1").await);
    wait_status(&state, &id, Status::Idle).await;
    let worktree = dir.path().join("worktrees").join(&id);
    std::fs::create_dir_all(&worktree).expect("a worktree path");
    record_worktree(&state, &id, &worktree);

    quota(&state).sweep().await;

    assert_eq!(
        row(&state, &id).status,
        Status::Idle,
        "a worktree exactly at its cap was stopped"
    );
    assert_eq!(row(&state, &id).status_reason, None);
    assert!(
        !lifecycle(&stub, &id).contains(&Call::Stop),
        "the backend was told to stop a session that is not over its cap"
    );
    assert!(
        reported(&state, &id).is_empty(),
        "an event was written for a session under its cap"
    );
}

#[tokio::test]
async fn a_measurement_that_cannot_run_stops_nothing() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("quota-unrunnable");
    let (state, stub) = harness(&dir, 1, failing_shim(&dir));
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "quota-unrunnable-1").await);
    wait_status(&state, &id, Status::Idle).await;
    let worktree = dir.path().join("worktrees").join(&id);
    std::fs::create_dir_all(&worktree).expect("a worktree path");
    record_worktree(&state, &id, &worktree);

    quota(&state).sweep().await;

    // `du` refused: the session is not measured, so it is not concluded against — a check that cannot run is
    // never a pass, and never a stop either.
    assert_eq!(row(&state, &id).status, Status::Idle);
    assert!(!lifecycle(&stub, &id).contains(&Call::Stop));
    assert!(reported(&state, &id).is_empty());
}

#[tokio::test]
async fn the_key_at_zero_builds_no_watchdog() {
    let dir = TempDir::new("quota-off");
    let state = state_in(&dir, capped(0));
    assert!(
        state.quota().is_none(),
        "session_disk_soft_gb = 0 built a watchdog anyway"
    );
}

#[tokio::test]
async fn a_running_turn_is_forced_stopped_after_the_grace() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("quota-forced");
    let log = dir.file("du.log");
    let shim = du_shim(&dir, 15 * 1024 * 1024, &log);
    // The hung agent rung 2 exists for: `clear_queue` and `abort` answer, and the turn does not stop. The
    // flag is a constructor argument, so it is set before the stub is cloned into the state.
    let stub = fast_stub().ignoring_abort();
    let mut state = with_stub(state_in(&dir, capped(10)), stub.clone());
    state.tools.du = shim;
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "quota-forced-1").await);
    wait_status(&state, &id, Status::Idle).await;
    let worktree = dir.path().join("worktrees").join(&id);
    std::fs::create_dir_all(&worktree).expect("a worktree path");
    record_worktree(&state, &id, &worktree);
    // A turn is in flight: the prompt is the handler's, and there is no runner here — which is exactly the
    // wedge rung 2 exists for (the agent that will not stop).
    drive(&state, &id, Trigger::Prompt, None).await;
    assert_eq!(row(&state, &id).status, Status::Running);

    quota(&state).sweep().await;

    // Rung 1 was asked (the queue cleared, the abort sent) and rung 2 is what ended it: `abort_ladder`'s row
    // with *this* policy's reason, so the row says `disk_quota` and not `forced_abort`.
    let calls = lifecycle(&stub, &id);
    assert!(calls.contains(&Call::ClearQueue), "{calls:?}");
    assert!(calls.contains(&Call::Abort), "{calls:?}");
    let stopped = row(&state, &id);
    assert_eq!(stopped.status, Status::Stopped);
    assert_eq!(stopped.status_reason.as_deref(), Some(DISK_QUOTA));
    wait_until("the stop reached the backend", || {
        lifecycle(&stub, &id).contains(&Call::Stop)
    })
    .await;
    wait_until("the cap's event is committed", || {
        !reported(&state, &id).is_empty()
    })
    .await;
    assert_eq!(reported(&state, &id)[0]["code"], DISK_QUOTA);
}

#[tokio::test]
async fn a_turn_that_aborts_politely_is_stopped_as_an_idle_session() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("quota-asked");
    let log = dir.file("du.log");
    let shim = du_shim(&dir, 15 * 1024 * 1024, &log);
    let (state, stub) = harness(&dir, 10, shim);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "quota-asked-1").await);
    wait_status(&state, &id, Status::Idle).await;
    let worktree = dir.path().join("worktrees").join(&id);
    std::fs::create_dir_all(&worktree).expect("a worktree path");
    record_worktree(&state, &id, &worktree);
    drive(&state, &id, Trigger::Prompt, None).await;

    // The turn ends itself while the ladder is waiting — the turn runner's own `abort_completed` ending — so
    // rung 1 worked and rung 2 is not needed. The session must still be stopped, and the row's reason must be
    // this policy's rather than the abort's.
    let ending = {
        let state = state.clone();
        let id = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            drive(
                &state,
                &id,
                Trigger::AbortCompleted,
                Some("abort_completed"),
            )
            .await;
        })
    };
    quota(&state).sweep().await;
    ending.await.expect("the ending task ran");

    let stopped = row(&state, &id);
    assert_eq!(stopped.status, Status::Stopped);
    assert_eq!(
        stopped.status_reason.as_deref(),
        Some(DISK_QUOTA),
        "the stop kept the abort's reason instead of its own"
    );
    assert!(
        lifecycle(&stub, &id).contains(&Call::Abort),
        "rung 1 was not asked"
    );
    wait_until("the stop reached the backend", || {
        lifecycle(&stub, &id).contains(&Call::Stop)
    })
    .await;
    wait_until("the cap's event is committed", || {
        !reported(&state, &id).is_empty()
    })
    .await;
}

#[tokio::test]
async fn one_crossing_is_one_event() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("quota-once");
    let log = dir.file("du.log");
    let shim = du_shim(&dir, 15 * 1024 * 1024, &log);
    let (state, _) = harness(&dir, 10, shim);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "quota-once-1").await);
    wait_status(&state, &id, Status::Idle).await;
    let worktree = dir.path().join("worktrees").join(&id);
    std::fs::create_dir_all(&worktree).expect("a worktree path");
    record_worktree(&state, &id, &worktree);

    quota(&state).sweep().await;
    quota(&state).sweep().await;
    wait_until("the cap's event is committed", || {
        !reported(&state, &id).is_empty()
    })
    .await;

    // The second pass found no candidate — a stopped session is not one — so the log carries one frame. The
    // stale-quota landmine (an event per tick for ever) is a consequence of the candidate rule, not a counter.
    assert_eq!(reported(&state, &id).len(), 1);
}

#[tokio::test]
async fn the_event_is_at_the_wire() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("quota-wire");
    let log = dir.file("du.log");
    let shim = du_shim(&dir, 15 * 1024 * 1024, &log);
    let (state, _) = harness(&dir, 10, shim);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "quota-wire-1").await);
    wait_status(&state, &id, Status::Idle).await;
    let worktree = dir.path().join("worktrees").join(&id);
    std::fs::create_dir_all(&worktree).expect("a worktree path");
    record_worktree(&state, &id, &worktree);
    quota(&state).sweep().await;
    wait_until("the cap's event is committed", || {
        !reported(&state, &id).is_empty()
    })
    .await;

    // The frame a client reads: the kind, the code and the two numbers, over the endpoint history uses.
    let page = support::fetch(
        served.addr,
        &format!("/api/sessions/{id}/events/history?before=1000000"),
    )
    .await;
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(
        page.header("content-type"),
        Some("application/json"),
        "the page is JSON"
    );
    let body: Value = serde_json::from_str(&page.body).expect("a page is JSON");
    let frame = body["events"]
        .as_array()
        .expect("a page carries events")
        .iter()
        .find(|event| event["type"] == "error")
        .expect("the page carries the cap's event");
    let payload: Value =
        serde_json::from_str(frame["payload"].as_str().expect("the payload is text"))
            .expect("the payload is JSON");
    assert_eq!(payload["code"], DISK_QUOTA);
    assert_eq!(payload["limit_gb"], 10);
    assert_eq!(payload["size_kib"], 15 * 1024 * 1024);
}

#[tokio::test]
async fn a_quota_stop_is_resumable() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("quota-resume");
    let log = dir.file("du.log");
    let shim = du_shim(&dir, 15 * 1024 * 1024, &log);
    let (state, _) = harness(&dir, 10, shim);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "quota-resume-1").await);
    wait_status(&state, &id, Status::Idle).await;
    let worktree = dir.path().join("worktrees").join(&id);
    std::fs::create_dir_all(&worktree).expect("a worktree path");
    record_worktree(&state, &id, &worktree);
    quota(&state).sweep().await;
    assert_eq!(row(&state, &id).status, Status::Stopped);

    // "Resumable after cleanup": the operator cleans up (the test's shim now reports a small tree) and asks
    // for the session back. A stopped row is the resume's, and the row walks back to `idle`.
    let text = format!(
        "POST /api/sessions/{id}/resume HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nContent-Length: 0\r\n\
         Connection: close\r\n\r\n"
    );
    let response = request(served.addr, &text).await;
    assert_eq!(response.status, 202, "{}", response.body);
    wait_status(&state, &id, Status::Idle).await;
    assert_eq!(
        row(&state, &id).status_reason,
        None,
        "the resume did not clear the reason"
    );
}

/// The fourth acceptance bullet, over the machine's own `du`: the cap counts `target/` and `node_modules/`.
#[test]
fn the_measurement_counts_target_and_node_modules() {
    let dir = TempDir::new("quota-counts");
    let worktree = dir.path().join("worktree");
    for sub in ["target", "node_modules", "src"] {
        std::fs::create_dir_all(worktree.join(sub)).expect("a subdirectory");
    }
    // 64 KiB in each: a build tree's two big directories, and one that would not be there in a fresh checkout.
    for (sub, name) in [
        ("target", "libholosmith.rlib"),
        ("node_modules", "package.tgz"),
        ("src", "main.rs"),
    ] {
        std::fs::write(worktree.join(sub).join(name), vec![0u8; 64 * 1024]).expect("a file");
    }

    let size = quota::measure(Path::new(quota::DU), &worktree).expect("the machine's du answers");
    // `du -sk` reports in whole KiB, and 192 KiB of content cannot be counted as less than 128 KiB by any
    // answer that sums both of the big directories; a `du` that excluded either would be under this bound.
    assert!(
        size >= 128,
        "the measurement left target/ or node_modules/ out: {size} KiB"
    );
    // And the reason `session_disk_soft_gb = 0` never reaches the comparison: a zero cap would call every
    // worktree over its limit, so "off" is `AppState::quota`'s `None` rather than a number the sweep hands to
    // `over` (`the_key_at_zero_builds_no_watchdog` is that half).
    assert!(quota::over(size, 0));
}

/// The three ways a measurement fails, and the argument vector it must ask for.
///
/// Both belong here rather than in the module's own tests: they write a shim and then exec it, and a *fork*
/// anywhere in the process inherits the open writing descriptor — which is the `ETXTBSY` race this binary's
/// `one_at_a_time` lock exists for. Nothing in the module's tests forks.
#[tokio::test]
async fn a_measurement_that_cannot_run_stops_nothing_and_asks_for_nothing_extra() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("quota-du-arms");
    let worktree = dir.path().join("worktree");
    std::fs::create_dir_all(&worktree).expect("a worktree path");

    // 1. A program that is not there. `Command` fails with `NotFound`, which must be a failure rather than
    //    "nothing to measure" — a check that cannot run is never a pass.
    let missing = quota::measure(Path::new("/nonexistent-holosmith-du"), &worktree);
    assert!(
        matches!(missing, Err(quota::DuError::Unrunnable(_))),
        "{missing:?}"
    );

    // 2. One that runs and refuses: its own words travel, because they are the operator's context.
    let refused = quota::measure(&failing_shim(&dir), &worktree);
    match refused {
        Err(quota::DuError::Refused(said)) => assert!(said.contains("cannot read it"), "{said}"),
        other => panic!("a refusing du answered {other:?}"),
    }

    // 3. One whose answer is not a `du -sk` answer: a word, or nothing at all.
    let babbling = dir.file("du-babbling");
    std::fs::write(&babbling, "#!/bin/sh\necho 'not a number at all'\n").expect("a shim");
    std::fs::set_permissions(
        &babbling,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("executable");
    let unprintable = quota::measure(&babbling, &worktree);
    assert!(
        matches!(unprintable, Err(quota::DuError::Unprintable(_))),
        "{unprintable:?}"
    );

    // 4. And the vector itself: one line, in kibibytes, with `--` before the path, and **no
    //    `--one-file-system`**. The recording shim is the only witness of the arguments — the inclusion test
    //    below cannot see the flag, because its tree is all on one filesystem.
    let log = dir.file("args.log");
    let recording = dir.file("du-recording");
    std::fs::write(
        &recording,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf '12\\t/work\\n'\n",
            log.display()
        ),
    )
    .expect("a shim");
    std::fs::set_permissions(
        &recording,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("executable");
    assert_eq!(quota::measure(&recording, &worktree), Ok(12));
    let asked = std::fs::read_to_string(&log).expect("the shim recorded its arguments");
    assert_eq!(
        asked.trim(),
        format!("-sk -- {}", worktree.display()),
        "the measurement asked for something else"
    );
}
