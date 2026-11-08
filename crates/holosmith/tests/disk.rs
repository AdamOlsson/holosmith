//! The disk floor at the wire (HS-062, spec item 14): "session creation and resume fail with 507 below
//! `min_free_gb`".
//!
//! Every test here owns a `df` the test wrote and a state whose run provisions containers as far as the floor
//! is concerned — `runtime.backend` is what [`holosmith::disk::guarded`] reads, exactly as it is what `serve`
//! built `Backends` from, while the *provisioning* backend stays the stub so a turn runs without a daemon. That
//! is the same seam `tests/preflight.rs` uses for the boot (`preflight::Tools`), pointed at the request paths.
//!
//! Two assertions per refusal, never one: the status alone would let a wrong `code` ship (ADR-0008 makes the
//! code the thing a client switches on), and a sentence nothing checks is a sentence that can stop naming the
//! numbers it exists to name.

use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use holosmith::backend::StubBackend;
use holosmith::config::{Config, Role};
use holosmith::disk::{self, Verdict};
use holosmith::http::error::DISK_FLOOR;
use holosmith::http::{self, AppState};
use holosmith::preflight::{self, Check, Outcome, Tools};
use holosmith::runtime::{Backend, Runtime};
use holosmith::sessions::state::{Transitioner, Trigger};
use holosmith::sessions::{Session, Status};
use serde_json::Value;

mod support;
use support::{PATIENCE, Response, Served, TempDir, fast_stub, request, state_in, with_stub};

const GRACE: Duration = Duration::from_millis(100);

/// `Config::default().control_origin`, which is what the default guards expect.
const ORIGIN: &str = "http://localhost:8080";

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

const PROMPT: &str = r#"{"prompt":"read the app shell"}"#;

/// The floor every test that wants one uses: `Config::default().min_free_gb`.
const FLOOR: u64 = 20;

/// Kibibytes per gibibyte, so an answer can be written as a number of GB.
const KIB_PER_GIB: u64 = 1024 * 1024;

/// Hold this for the whole body of a test here: one test at a time.
///
/// `exec` of a file that *any* process holds open for writing fails with `ETXTBSY`, and `Command`'s fork
/// happens on another thread's behalf, so a shim written a moment earlier can still be open when the `df` this
/// file planted is executed. `tests/preflight.rs` measured it at roughly 3 runs in 40 before this lock; every
/// test here writes a shim and then runs it, and nothing else in this binary forks, so serialising each whole
/// body is the whole fix.
async fn one_at_a_time() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

/// A POSIX `df -Pk` answer with `available_gb` free.
///
/// Two lines and four fields, which is what `-P` promises: the reader takes the fourth field of the second line,
/// so an answer written any other way would be testing the format rather than the floor.
fn answer(available_gb: u64) -> String {
    format!(
        "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
         /dev/mapper/root 1000000000 1 {} 1% /\n",
        available_gb * KIB_PER_GIB
    )
}

/// A `df` that prints whatever `<dir>/df-answer` holds, and the path of that file.
///
/// The stand-in is a file the *test* rewrites, which is what lets one server boot on a healthy machine and then
/// cross the floor: the boot refuses below it (HS-024), so a case about the request paths cannot simply start
/// with a small disk. Nothing here talks to a real filesystem; `cat` is all the program does.
fn stand_in(dir: &TempDir, available_gb: u64) -> (PathBuf, PathBuf) {
    let answer_path = dir.write("df-answer", &answer(available_gb));
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("the shim directory is creatable");
    let df = bin.join("df");
    let mut script = std::fs::File::create(&df).expect("a shim is writable");
    script
        .write_all(format!("#!/bin/sh\ncat {}\n", answer_path.display()).as_bytes())
        .expect("the shim is written");
    drop(script);
    let mut permissions = std::fs::metadata(&df)
        .expect("the shim exists")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&df, permissions).expect("a shim is executable");
    (df, answer_path)
}

/// Rewrite the answer the stand-in prints — the test's whole floor control.
fn plant(answer_path: &std::path::Path, available_gb: u64) {
    std::fs::write(answer_path, answer(available_gb)).expect("the answer is writable");
}

/// A state whose run is measured by the floor, over `dir`'s own data root.
///
/// `runtime` says Docker because that is the question [`disk::guarded`] asks and the answer `serve` would give a
/// real machine; `backend` stays the stub, so no container is ever created and a turn still runs.
fn floored_state(dir: &TempDir, df: PathBuf, min_free_gb: u64) -> AppState {
    let data_root = dir.path().join("data");
    std::fs::create_dir_all(&data_root).expect("the data root is creatable");
    let config = Config {
        data_root,
        min_free_gb,
        ..Config::default()
    };
    let mut state = with_stub(
        state_in(dir, config),
        StubBackend::new(Duration::from_millis(50)),
    );
    state.runtime = Runtime::started(Backend::Docker);
    state.tools = Tools {
        git: PathBuf::from("git"),
        df,
        // The soft cap's program (HS-063); this test is about `df`.
        du: PathBuf::from("du"),
    };
    state
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

async fn post_resume(addr: SocketAddr, id: &str) -> Response {
    let text = format!(
        "POST /api/sessions/{id}/resume HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nContent-Length: 0\r\n\
         Connection: close\r\n\r\n"
    );
    request(addr, &text).await
}

async fn post_prompt(addr: SocketAddr, id: &str) -> Response {
    let text = format!(
        "POST /api/sessions/{id}/prompt HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{PROMPT}",
        PROMPT.len()
    );
    request(addr, &text).await
}

/// The id a 202's body names.
fn created_id(response: &Response) -> String {
    assert_eq!(response.status, 202, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("a create answers JSON");
    body["id"]
        .as_str()
        .expect("the response names an id")
        .to_string()
}

/// The floor's refusal, asserted as three facts: the status, the envelope's code, and the sentence naming the
/// numbers. The data root's path is in the sentence too, because two filesystems on one machine is exactly when
/// a number without a place is unhelpful.
fn assert_floor(response: &Response, available_gb: u64, data_root: &std::path::Path) {
    assert_eq!(response.status, 507, "{}", response.body);
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "{}",
        response.body
    );
    let body: Value = serde_json::from_str(&response.body).expect("an envelope is JSON");
    assert_eq!(body["code"], DISK_FLOOR, "{}", response.body);
    let message = body["message"].as_str().expect("a message is a string");
    assert!(
        message.contains(&format!("has {available_gb} GB free")),
        "the refusal does not name the free space: {message}"
    );
    assert!(
        message.contains(&format!("min_free_gb is {FLOOR}")),
        "the refusal does not name the floor: {message}"
    );
    assert!(
        message.contains(&data_root.display().to_string()),
        "the refusal does not name the filesystem: {message}"
    );
    assert!(
        message.contains("free some space"),
        "the refusal does not name the fix: {message}"
    );
}

fn row(state: &AppState, id: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .get(id)
        .expect("the row reads")
        .expect("the session exists")
}

fn session_count(state: &AppState) -> usize {
    let database = state.database.lock().expect("the lock is not poisoned");
    database.sessions().list(None).expect("the rows read").len()
}

async fn wait_status(state: &AppState, id: &str, want: Status) {
    let deadline = std::time::Instant::now() + PATIENCE;
    while row(state, id).status != want {
        assert!(
            std::time::Instant::now() < deadline,
            "the session did not reach {want:?} within {PATIENCE:?}: {:?}",
            row(state, id).status
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn the_boot_and_the_request_paths_read_one_number() {
    // The acceptance's third bullet — "`doctor` reports the same measurement, so the two never disagree" — as a
    // measurement rather than a promise: one `df` answer, handed to the boot's check and to the reader the two
    // request paths use, must be one number.
    let _one = one_at_a_time().await;
    let dir = TempDir::new("disk-agree");
    let (df, _answer_path) = stand_in(&dir, 5);
    let data_root = dir.path().join("data");
    std::fs::create_dir_all(&data_root).expect("the data root is creatable");
    let config = Config {
        data_root,
        min_free_gb: FLOOR,
        // The daemon's check is not this test's subject, and a socket nobody answers must not cost three
        // seconds: an unreachable path fails at once.
        docker_host: Some("unix:///nonexistent-holosmith.sock".to_string()),
        ..Config::default()
    };
    let findings = preflight::report_with(
        &config,
        &Tools {
            git: PathBuf::from("git"),
            df: df.clone(),
            // The soft cap's program (HS-063); this test is about `df`.
            du: PathBuf::from("du"),
        },
    );
    let disk_finding = findings
        .iter()
        .find(|finding| finding.check == Check::Disk)
        .expect("the report lists a disk check");
    assert_eq!(disk_finding.outcome, Outcome::Fail);
    assert!(
        disk_finding.sentence.contains("has 5 GB free"),
        "the boot's sentence does not name the number the answer held: {}",
        disk_finding.sentence
    );
    assert_eq!(
        disk::measure(&df, &config.data_root),
        Ok(5),
        "the request paths would read a second number"
    );
    assert_eq!(disk::verdict(5, FLOOR), Verdict::Short);

    // And the number a *handler* computes is the same one, which is the half a direct `measure` call cannot see:
    // a refusal with its own parser and its own divisor would pass everything above and fail here.
    let mut state = with_stub(state_in(&dir, config), fast_stub());
    state.runtime = Runtime::started(Backend::Docker);
    state.tools = Tools {
        git: PathBuf::from("git"),
        df,
        // The soft cap's program (HS-063); this test is about `df`.
        du: PathBuf::from("du"),
    };
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let refused = post_create(served.addr, "disk-floor-agree").await;
    assert_floor(&refused, 5, &state.config.data_root);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_create_below_the_floor_is_a_507_and_creates_nothing() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("disk-create");
    let (df, _answer_path) = stand_in(&dir, 5);
    let state = floored_state(&dir, df, FLOOR);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let response = post_create(served.addr, "disk-floor-create").await;
    assert_floor(&response, 5, &state.config.data_root);

    // "The check runs before any worktree or network is created": no row, so nothing to clean up and nothing
    // for a provisioner to walk. Asserted as the two facts the sentence cannot be trusted for.
    assert_eq!(session_count(&state), 0, "the refusal wrote a row");
    let worktrees = state.config.data_root.join(".worktrees");
    assert!(
        !worktrees.exists()
            || std::fs::read_dir(&worktrees)
                .expect("readable")
                .next()
                .is_none(),
        "the refusal made a worktree"
    );
    // And the list a client polls still answers, so the refusal is not the whole server.
    let list = support::fetch(served.addr, "/api/sessions").await;
    assert_eq!(list.status, 200, "{}", list.body);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_resume_below_the_floor_is_a_507_and_leaves_the_row_alone() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("disk-resume");
    let (df, answer_path) = stand_in(&dir, 500);
    let state = floored_state(&dir, df, FLOOR);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    // A `stopped` row, reached the way the policies reach one: the API has no stop endpoint (HS-018's note).
    let id = created_id(&post_create(served.addr, "disk-floor-resume").await);
    wait_status(&state, &id, Status::Idle).await;
    Transitioner::new(std::sync::Arc::clone(&state.database), state.events.clone())
        .apply(&id, Trigger::IdleStop, Some("idle_stop"))
        .await
        .expect("the move is in the table");
    assert_eq!(row(&state, &id).status, Status::Stopped);

    // The floor is crossed *after* the row exists, which is the story's shape: only new starts are refused.
    plant(&answer_path, 5);
    let response = post_resume(served.addr, &id).await;
    assert_floor(&response, 5, &state.config.data_root);

    // Nothing moved: a refused resume must not strand the row in `starting`, and it must still be resumable
    // once the operator has freed the space.
    assert_eq!(
        row(&state, &id).status,
        Status::Stopped,
        "the refused resume moved the row"
    );
    plant(&answer_path, 500);
    let resumed = post_resume(served.addr, &id).await;
    assert_eq!(resumed.status, 202, "{}", resumed.body);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_replay_of_a_key_that_names_a_row_answers_the_row_not_the_floor() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("disk-replay");
    let (df, answer_path) = stand_in(&dir, 500);
    let state = floored_state(&dir, df, FLOOR);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let first = post_create(served.addr, "disk-floor-replay").await;
    let id = created_id(&first);

    // The machine fills up between a request and its retry, which is the case `create_locked`'s order exists
    // for: the key's lookup is before every refusal, so the retry is answered from the row it already has and
    // the operator does not lose a session that was created while there was room.
    plant(&answer_path, 5);
    let replay = post_create(served.addr, "disk-floor-replay").await;
    assert_eq!(replay.status, 202, "{}", replay.body);
    assert_eq!(created_id(&replay), id);
    assert_eq!(session_count(&state), 1, "the replay created a second row");

    // A *new* key is a new start, and that is what the floor refuses.
    let fresh = post_create(served.addr, "disk-floor-replay-2").await;
    assert_floor(&fresh, 5, &state.config.data_root);
    assert_eq!(session_count(&state), 1, "the refusal wrote a row");
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_turn_that_is_already_running_settles_when_the_floor_is_crossed() {
    let _one = one_at_a_time().await;
    let dir = TempDir::new("disk-turn");
    let (df, answer_path) = stand_in(&dir, 500);
    let state = floored_state(&dir, df, FLOOR);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "disk-floor-turn").await);
    wait_status(&state, &id, Status::Idle).await;

    let prompted = post_prompt(served.addr, &id).await;
    assert_eq!(prompted.status, 202, "{}", prompted.body);
    // The floor is crossed **while** the turn is in flight, which is the only version of this that means
    // anything: the row is `running` at the moment the answer is planted, so everything after it happens under a
    // machine that would refuse a new start. Nothing about a running turn reads the disk, so the turn settles by
    // itself — the story's fourth line as a measurement.
    wait_status(&state, &id, Status::Running).await;
    plant(&answer_path, 5);
    wait_status(&state, &id, Status::Idle).await;

    // The turn ended in its own state rather than the floor's: an interrupted turn would be `error` (or
    // `stopped`), and the same machine still refuses the next *start*.
    assert_eq!(row(&state, &id).status, Status::Idle);
    assert_floor(
        &post_create(served.addr, "disk-floor-turn-2").await,
        5,
        &state.config.data_root,
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_floor_of_zero_is_no_floor() {
    let _one = one_at_a_time().await;
    // ADR-0068: `min_free_gb = 0` is legal and means "no floor", because the comparison is `available >= min`.
    // A story that asked for a refusal does not get to invent a config rule, and `tests/serve_cli.rs` already
    // writes `0` to say exactly this.
    let dir = TempDir::new("disk-zero");
    let (df, _answer_path) = stand_in(&dir, 0);
    let state = floored_state(&dir, df, 0);
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let response = post_create(served.addr, "disk-floor-zero").await;
    assert_eq!(response.status, 202, "{}", response.body);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_stub_run_is_not_measured_at_all() {
    let _one = one_at_a_time().await;
    // Every other test binary in this crate creates sessions on the stub backend with the default config, on a
    // sandbox whose `/tmp` has 3 GB and a floor of 20. This is why they all still pass: `disk::guarded` asks the
    // *run's* backend, and a stub session starts no container and fills no filesystem (ADR-0068).
    let dir = TempDir::new("disk-stub");
    let (df, _answer_path) = stand_in(&dir, 0);
    let data_root = dir.path().join("data");
    std::fs::create_dir_all(&data_root).expect("the data root is creatable");
    let config = Config {
        data_root,
        min_free_gb: 1_000,
        ..Config::default()
    };
    let mut state = with_stub(state_in(&dir, config), fast_stub());
    state.tools = Tools {
        git: PathBuf::from("git"),
        df,
        // The soft cap's program (HS-063); this test is about `df`.
        du: PathBuf::from("du"),
    };
    // `state_in` leaves `runtime` on the stub, which is what `serve --backend stub` would build.
    assert_eq!(state.runtime.backend, Backend::Stub);
    assert_eq!(state.config.role, Role::Both);
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let response = post_create(served.addr, "disk-floor-stub").await;
    assert_eq!(response.status, 202, "{}", response.body);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_create_at_the_cap_refuses_the_floor_instead_of_evicting() {
    let _one = one_at_a_time().await;
    // `create_locked` raises its refusal between the idempotency key and the cap, so a create that cannot run
    // answers its own refusal rather than stopping somebody else's session (HS-026's rule, ADR-0068 D1). One live
    // session at a cap of one is what makes that order observable: raised after the cap, this create would evict
    // the idle session on its way to saying no.
    let dir = TempDir::new("disk-cap");
    let (df, answer_path) = stand_in(&dir, 500);
    let mut state = floored_state(&dir, df, FLOOR);
    state.config.max_live_sessions = 1;
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "disk-floor-cap").await);
    wait_status(&state, &id, Status::Idle).await;

    plant(&answer_path, 5);
    let refused = post_create(served.addr, "disk-floor-cap-2").await;
    assert_floor(&refused, 5, &state.config.data_root);
    assert_eq!(
        row(&state, &id).status,
        Status::Idle,
        "the floor evicted the idle session to make room for work that will not happen"
    );
    assert_eq!(session_count(&state), 1);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_floor_of_zero_does_not_need_a_df_at_all() {
    let _one = one_at_a_time().await;
    // ADR-0068 D4: `min_free_gb = 0` is *no floor*, so there is nothing to compare and nothing to measure — which
    // has to include the reader itself. Measuring first and then comparing would answer `500 internal` for a
    // machine with no `df` over a floor it does not have.
    let dir = TempDir::new("disk-zero-no-df");
    let data_root = dir.path().join("data");
    std::fs::create_dir_all(&data_root).expect("the data root is creatable");
    let config = Config {
        data_root,
        min_free_gb: 0,
        ..Config::default()
    };
    let mut state = with_stub(state_in(&dir, config), fast_stub());
    state.runtime = Runtime::started(Backend::Docker);
    state.tools = Tools {
        git: PathBuf::from("git"),
        df: PathBuf::from("/nonexistent-holosmith-df"),
        // The soft cap's program (HS-063); this test is about `df`.
        du: PathBuf::from("du"),
    };
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let response = post_create(served.addr, "disk-floor-zero-no-df").await;
    assert_eq!(response.status, 202, "{}", response.body);
    served.finish().await.expect("a clean shutdown");
}
