//! The drain: the socket, the wait, the stop and the refusals (spec item 10 step 3).
//!
//! Every test drives the production values — [`drain::listen`], [`drain::spawn`], [`drain::Drain::run`] and, for
//! the refusals, the real `App` over HTTP — so what is asserted is the behaviour an operator meets rather than a
//! second implementation of it. Rows are seeded through the transition table (`create_accepted`, `pi_ready`,
//! `prompt`) rather than through a backend turn, because the drain's subject is the **row**: it waits for a
//! `running` row and stops an `idle` one, and neither question needs a stub to pace anything.
//!
//! Each test owns its `TempDir` and its own `Database`, so nothing here shares a file with another test
//! (ADR-0009's measured same-process "database is locked" trap).

use std::time::Duration;

use holosmith::backend::{Call, StubBackend};
use holosmith::config::Config;
use holosmith::drain::{self, Drain, Ending, LineSink, Request, SERVER_RESTART};
use holosmith::http::{self, AppState, Guards};
use holosmith::sessions::state::Trigger;
use holosmith::sessions::{NewSession, Session, Status};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

mod support;
use support::{Response, Served, TempDir, fast_stub, fetch, request, state_in, with_stub};

const GRACE: Duration = Duration::from_secs(10);

/// `Config::default().control_origin`, which is what the default guards expect.
const ORIGIN: &str = "http://localhost:8080";

/// Every header a mutating request needs (ADR-0010).
const GUARDS: &[(&str, &str)] = &[
    ("Host", "127.0.0.1"),
    ("Origin", ORIGIN),
    ("Content-Type", "application/json"),
    ("X-Holosmith-Client", "1"),
];

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

/// A state with a fast stub, so the tests that go through a route reach it without a 60-second step.
fn state(dir: &TempDir) -> AppState {
    with_stub(state_in(dir, Config::default()), fast_stub())
}

/// Seed a row and walk it to `status` through the table, which is the only way a state changes (ADR-0016).
///
/// `status` is one of `starting`, `idle` or `running` — the three a drain's wait and stop distinguish — and the
/// trigger sequence is the create's own (`none → starting → idle`) plus a prompt when a running row is asked for.
async fn seed(state: &AppState, status: Status) -> Session {
    let session = {
        let database = state.database.lock().expect("the database lock");
        database
            .sessions()
            .create(
                &NewSession {
                    project: "holosmith".to_string(),
                    model: "claude-sonnet-4.5".to_string(),
                    thinking: "high".to_string(),
                    network: false,
                    client_key: None,
                    base_commit: None,
                },
                holosmith::clock::unix_now(),
            )
            .expect("a session row")
    };
    // The row is inserted `starting` — the table's `none -> starting` row is what the *insert* is, not a
    // second write — so a seed walks on from there.
    let transitioner = state.transitioner();
    if status == Status::Starting {
        return read(state, &session.id);
    }
    transitioner
        .apply(&session.id, Trigger::PiReady, None)
        .await
        .expect("provisioning finishes");
    if status == Status::Idle {
        return read(state, &session.id);
    }
    transitioner
        .apply(&session.id, Trigger::Prompt, None)
        .await
        .expect("an idle session takes a prompt");
    read(state, &session.id)
}

/// The row as the database holds it, after the writer's events have been committed by the transition itself.
fn read(state: &AppState, id: &str) -> Session {
    let database = state.database.lock().expect("the database lock");
    database
        .sessions()
        .get(id)
        .expect("the row reads")
        .expect("the row exists")
}

/// Move a row with one trigger, as the turn runner would when a turn settles.
async fn apply(state: &AppState, id: &str, trigger: Trigger) {
    state
        .transitioner()
        .apply(id, trigger, None)
        .await
        .expect("the transition is in the table");
}

/// A socket a test can talk to, and the task that accepts on it.
struct Socket {
    path: std::path::PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl Socket {
    fn start(dir: &TempDir, drain: Drain) -> Self {
        let admin = dir.file("run/holosmith");
        let listener = drain::listen(&admin).expect("the admin socket binds");
        let path = drain::socket_path(&admin);
        Self {
            path,
            task: drain::spawn(listener, drain),
        }
    }

    /// Send one request line and read every line the answer brought, in order.
    async fn ask(&self, request: &str) -> Vec<String> {
        let stream = UnixStream::connect(&self.path)
            .await
            .expect("the admin socket accepts");
        let mut stream = BufReader::new(stream);
        stream
            .get_mut()
            .write_all(request.as_bytes())
            .await
            .expect("the request is written");
        let mut lines = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            match stream.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => lines.push(line.trim_end().to_string()),
                // A server that refuses a malformed request closes with the rest of the line unread, and the
                // kernel answers that close with a reset. The answer itself arrived first, and the assertions
                // below are about it.
                Err(_) => break,
            }
        }
        lines
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The terminal line's word, or a panic naming what came instead.
fn result_of(lines: &[String]) -> &str {
    let last = lines
        .last()
        .unwrap_or_else(|| panic!("the answer is empty"));
    last.strip_prefix("result: ")
        .unwrap_or_else(|| panic!("the answer's last line is not a result: {lines:?}"))
}

// ----------------------------------------------------------------------------------------------------------
// The sequence
// ----------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_drain_with_nothing_running_stops_the_idle_sessions() {
    let dir = TempDir::new("drain-idle");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let first = seed(&state, Status::Idle).await;
    let second = seed(&state, Status::Idle).await;

    let (report, mut lines) = sink();
    let ending = state.drainer().run(Duration::from_secs(30), &report).await;
    let lines = drain_lines(&mut lines);

    assert_eq!(ending, Ending::Drained);
    assert_eq!(lines[0], "state: draining");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("stopped: "))
            .count(),
        1,
        "one stop line, not one per session: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "stopped: 2 sessions with reason server_restart"),
        "the report says how many and why: {lines:?}"
    );
    assert_eq!(result_of(&lines), "drained");

    for session in [&first, &second] {
        let row = read(&state, &session.id);
        assert_eq!(row.status, Status::Stopped, "{}", session.id);
        assert_eq!(row.status_reason.as_deref(), Some(SERVER_RESTART));
        assert!(row.stopped_at.is_some(), "a stopped row records when");
    }
    // The container stop is a task of its own (ADR-0024's shape), so it is waited for rather than read once.
    wait_for_stops(&stub, 2).await;
}

/// Break the database's schema, from a second connection on the same file, so one query fails.
///
/// The subject of these two tests is what the drain says when it **cannot look** — the arm that used to report
/// `drained` and exit 0 — and a schema the queries no longer fit is the honest way to reach it.
fn break_schema(dir: &TempDir, statement: &str) {
    let connection = rusqlite::Connection::open(dir.database()).expect("the database opens");
    connection
        .execute_batch(statement)
        .expect("the schema change applies");
}

/// It cannot count the turns: one terminal line, `refused`, and no sentence claiming the wait ran out.
///
/// `wait_for_turns` used to send its own `result: refused` and then return "not quiet", which `run` read as an
/// expiry — so the client (last terminal line wins) saw a false "the wait ran out after 10m with a turn still
/// running" and exited 1 for the wrong reason.
#[tokio::test(flavor = "multi_thread")]
async fn a_wait_that_cannot_count_the_turns_is_one_refused_line() {
    let dir = TempDir::new("drain-unreadable");
    let state = state(&dir);
    break_schema(&dir, "DROP TABLE sessions");

    let (report, mut lines) = sink();
    let ending = state.drainer().run(Duration::from_secs(30), &report).await;
    let lines = drain_lines(&mut lines);

    assert_eq!(ending, Ending::Refused);
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("result: "))
            .count(),
        1,
        "one terminal line: {lines:?}"
    );
    assert_eq!(result_of(&lines), "refused");
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("note: the sessions could not be read")),
        "the report says what it could not do: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("the wait ran out")),
        "a database that cannot be read is not an expiry: {lines:?}"
    );
    assert!(
        state.drain.is_draining(),
        "an unanswerable drain still leaves the flag on"
    );
}

/// It can count the turns but not list the rows it has to stop: `refused`, never `drained` and never a count.
///
/// `stop_idle` used to fold the read's failure into the sentence `no session could be read` and let `run` report
/// `drained` — exit 0, "the machine is quiet, go", over a machine whose idle sessions were still live.
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_phase_that_cannot_list_the_rows_is_refused_rather_than_quiet() {
    let dir = TempDir::new("drain-unlistable");
    let state = state(&dir);
    seed(&state, Status::Idle).await;
    // `counts_by_status` reads `status` and the count; `list` reads every column of `COLUMNS`. Renaming one of
    // the others is the smallest break that the wait survives and the stop's read does not.
    break_schema(
        &dir,
        "ALTER TABLE sessions RENAME COLUMN project TO project_renamed",
    );

    let (report, mut lines) = sink();
    let ending = state.drainer().run(Duration::from_secs(30), &report).await;
    let lines = drain_lines(&mut lines);

    assert_eq!(ending, Ending::Refused);
    assert_eq!(result_of(&lines), "refused");
    assert!(
        !lines.iter().any(|line| line.starts_with("stopped: ")),
        "nothing was stopped, so nothing may be counted: {lines:?}"
    );
}

/// A sink and the receiver a test reads the sequence's lines from.
fn sink() -> (LineSink, tokio::sync::mpsc::UnboundedReceiver<String>) {
    LineSink::new()
}

/// Wait until the stub has recorded `count` container stops.
async fn wait_for_stops(stub: &StubBackend, count: usize) {
    for _ in 0..200 {
        if stub
            .calls()
            .iter()
            .filter(|call| **call == Call::Stop)
            .count()
            >= count
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "the drain did not stop {count} containers: {:?}",
        stub.calls()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wait_is_the_running_rows_and_the_stop_comes_after_it() {
    let dir = TempDir::new("drain-wait");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let running = seed(&state, Status::Running).await;
    let idle = seed(&state, Status::Idle).await;

    let (report, mut lines) = sink();
    let drain = state.drainer().clone();
    let sequence = tokio::spawn(async move { drain.run(Duration::from_secs(30), &report).await });

    // Two facts while the turn is in flight, both of which the ordering argument depends on: the drain has not
    // finished, and it has not stopped the idle session either — "waits for running turns, **then** stops idle
    // sessions" is a sequence, and a stop that happened during the wait would be a different machine.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        !sequence.is_finished(),
        "the drain finished with a turn running"
    );
    assert_eq!(read(&state, &idle.id).status, Status::Idle);
    assert_eq!(read(&state, &running.id).status, Status::Running);
    let seen: Vec<String> = drain_lines(&mut lines);
    assert!(
        seen.iter()
            .any(|line| line == "waiting: 1 turn still running"),
        "the wait is reported: {seen:?}"
    );
    assert!(
        !seen.iter().any(|line| line.starts_with("stopped: ")),
        "nothing is stopped while a turn runs: {seen:?}"
    );

    // The turn ends the way a real one does, and the drain finishes on its own.
    apply(&state, &running.id, Trigger::AgentSettled).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), sequence)
            .await
            .expect("the drain finishes once the turn ends")
            .expect("the sequence does not panic"),
        Ending::Drained
    );
    for session in [&running, &idle] {
        let row = read(&state, &session.id);
        assert_eq!(row.status, Status::Stopped, "{}", session.id);
        assert_eq!(row.status_reason.as_deref(), Some(SERVER_RESTART));
    }
    wait_for_stops(&stub, 2).await;
}

/// Whatever the channel holds right now.
fn drain_lines(lines: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> Vec<String> {
    let mut seen = Vec::new();
    while let Ok(line) = lines.try_recv() {
        seen.push(line);
    }
    seen
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_wait_leaves_the_drain_on_and_stops_nothing() {
    let dir = TempDir::new("drain-expired");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let running = seed(&state, Status::Running).await;
    let idle = seed(&state, Status::Idle).await;

    let (report, mut lines) = sink();
    let ending = state
        .drainer()
        .run(Duration::from_millis(200), &report)
        .await;

    assert_eq!(ending, Ending::Expired);
    let seen = drain_lines(&mut lines);
    assert_eq!(result_of(&seen), "expired");
    assert!(
        seen.iter()
            .any(|line| line.starts_with("note: the wait ran out after 200ms")),
        "the report says which wait ran out: {seen:?}"
    );
    assert!(seen.iter().any(|line| line.contains("still draining")));

    // Acceptance bullet 4: the state is *left* on rather than silently succeeding.
    assert!(
        state.drain.is_draining(),
        "the expiry must not clear the flag"
    );
    assert_eq!(read(&state, &running.id).status, Status::Running);
    assert_eq!(read(&state, &idle.id).status, Status::Idle);
    assert!(
        !stub.calls().contains(&Call::Stop),
        "an expired drain stops nothing: {:?}",
        stub.calls()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_starting_session_is_reported_and_left_alone() {
    let dir = TempDir::new("drain-starting");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let starting = seed(&state, Status::Starting).await;
    let idle = seed(&state, Status::Idle).await;

    let (report, mut lines) = sink();
    // A short wait: a `starting` row must not hold the drain up, which is the whole claim.
    let ending = state
        .drainer()
        .run(Duration::from_millis(200), &report)
        .await;

    assert_eq!(ending, Ending::Drained);
    let seen = drain_lines(&mut lines);
    assert!(
        seen.iter()
            .any(|line| line.starts_with("note: 1 session is starting and left alone")),
        "the machine says what it is not stopping: {seen:?}"
    );
    assert_eq!(read(&state, &starting.id).status, Status::Starting);
    assert_eq!(read(&state, &idle.id).status, Status::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_drain_joins_rather_than_starting_again() {
    let dir = TempDir::new("drain-second");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let running = seed(&state, Status::Running).await;

    let socket = Socket::start(&dir, state.drainer());
    let first = socket.ask("drain 100\n").await;
    assert_eq!(result_of(&first), "expired");
    assert_eq!(first[0], "state: draining");

    let second = socket.ask("drain 300\n").await;
    assert_eq!(
        second[0], "state: already draining",
        "a second drain says the machine was already draining: {second:?}"
    );
    // The first drain's expiry stopped nothing, and the second one still had a turn to wait for, so it expired
    // too — the point is that it *joined* the wait rather than reporting a fresh drain.
    assert_eq!(result_of(&second), "expired");
    apply(&state, &running.id, Trigger::AgentSettled).await;
    let third = socket.ask("drain 5000\n").await;
    assert_eq!(result_of(&third), "drained");
    assert_eq!(read(&state, &running.id).status, Status::Stopped);
}

// ----------------------------------------------------------------------------------------------------------
// The socket
// ----------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_request_that_is_not_a_drain_is_refused_with_a_sentence() {
    let dir = TempDir::new("drain-protocol");
    let state = state(&dir);
    let socket = Socket::start(&dir, state.drainer());

    let refused = socket.ask("status\n").await;
    assert_eq!(result_of(&refused), "refused");
    assert!(
        refused[0].contains("the only request this socket answers"),
        "{refused:?}"
    );

    let refused = socket.ask("drain\n").await;
    assert_eq!(result_of(&refused), "refused");

    let refused = socket.ask(&format!("drain {}\n", "6".repeat(120))).await;
    assert_eq!(result_of(&refused), "refused");
    assert!(
        refused[0].contains("longer than this socket reads"),
        "a truncated line must not be parsed as a smaller wait: {refused:?}"
    );

    // The flag was not set by any of them: a refused request changes nothing.
    assert!(!state.drain.is_draining());
}

#[test]
fn the_request_line_is_one_verb_and_a_number_of_milliseconds() {
    assert_eq!(
        Request::parse("drain 600000\n").expect("a drain line parses"),
        Request::Drain(Duration::from_secs(600))
    );
    assert_eq!(
        Request::parse("drain 1500").expect("a line without a newline parses too"),
        Request::Drain(Duration::from_millis(1500))
    );
    for line in [
        "status",
        "drain",
        "drain x",
        "GET / HTTP/1.1",
        "",
        "drain -1",
    ] {
        assert!(
            Request::parse(line).is_err(),
            "{line:?} must be refused rather than guessed at"
        );
    }
}

#[tokio::test]
async fn a_stale_socket_is_replaced_and_a_live_one_and_a_file_are_not() {
    let dir = TempDir::new("drain-stale");
    let admin = dir.file("run/holosmith");
    std::fs::create_dir_all(&admin).expect("the admin directory is creatable");
    let path = drain::socket_path(&admin);

    // A server that died left its socket file: binding again must work, or a killed demo would wedge the
    // machine's admin socket until someone found the file.
    let dead = std::os::unix::net::UnixListener::bind(&path).expect("the stale socket binds");
    drop(dead);
    assert!(path.exists(), "a dropped listener leaves its socket file");
    let listener = drain::listen(&admin).expect("a dead server's socket is replaced");
    assert!(
        std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&path)
                .expect("the socket exists")
                .permissions()
        ) & 0o777
            == 0o600,
        "the socket is the service user's"
    );

    // A live one belongs to that server, and this run must not take its clients away.
    let sentence = drain::listen(&admin).expect_err("a live socket is not replaced");
    assert!(
        sentence.contains("already answered by a running server"),
        "{sentence}"
    );
    drop(listener);

    // Anything that is not a socket is an operator's file: a sentence, not a delete.
    std::fs::remove_file(&path).expect("the test's socket is removable");
    std::fs::write(&path, "keep me").expect("a file is writable");
    let sentence = drain::listen(&admin).expect_err("a regular file is not a socket");
    assert!(sentence.contains("is not a socket"), "{sentence}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file is still there"),
        "keep me"
    );
}

#[tokio::test]
async fn the_directory_is_created_0700_and_an_operators_mode_is_left_alone() {
    let dir = TempDir::new("drain-mode");
    let admin = dir.file("run/holosmith");
    let listener = drain::listen(&admin).expect("the directory is created");
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&admin)
            .expect("the directory exists")
            .permissions(),
    ) & 0o777;
    assert_eq!(
        mode, 0o700,
        "a directory this run made is 0700, like step 2's"
    );

    // The first listener is still bound, so it is dropped before the second bind: a live socket is refused by
    // the rule the test above asserts, and that is not what this one is about.
    drop(listener);
    std::fs::set_permissions(&admin, std::os::unix::fs::PermissionsExt::from_mode(0o750))
        .expect("the mode is settable");
    let _listener = drain::listen(&admin).expect("the socket binds again");
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&admin)
            .expect("the directory exists")
            .permissions(),
    ) & 0o777;
    assert_eq!(mode, 0o750, "an operator's own mode is theirs");
}

// ----------------------------------------------------------------------------------------------------------
// The refusals, over HTTP
// ----------------------------------------------------------------------------------------------------------

/// One request with the guard headers a mutating call needs.
async fn guarded(addr: std::net::SocketAddr, method: &str, path: &str, body: &str) -> Response {
    let mut text = format!("{method} {path} HTTP/1.1\r\n");
    for (name, value) in GUARDS {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    // A create needs one, and a request that is not a create ignores it (ADR-0012's own header rules).
    text.push_str("Idempotency-Key: drain-test\r\n");
    text.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    request(addr, &text).await
}

/// The ADR-0008 envelope: the status, the content type and exactly the code.
fn assert_refusal(response: &Response, status: u16, code: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert!(
        response.body.contains(&format!("\"code\":\"{code}\"")),
        "{}",
        response.body
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn while_draining_the_create_and_the_prompt_are_refused_and_the_reads_are_not() {
    let dir = TempDir::new("drain-refusals");
    let state = state(&dir);
    let idle = seed(&state, Status::Idle).await;
    let running = seed(&state, Status::Running).await;
    let stopped = seed(&state, Status::Idle).await;
    apply(&state, &stopped.id, Trigger::Drain).await;
    let served = Served::start(http::router(state.clone()), GRACE).await;

    state.drain.begin();

    // The two the spec names (item 10 step 3), each with the envelope and the new code.
    let response = guarded(served.addr, "POST", "/api/sessions", BODY).await;
    assert_refusal(&response, 503, "draining");
    assert!(
        response.body.contains("holosmith drain --wait"),
        "the sentence names the drain and its wait: {}",
        response.body
    );
    let response = guarded(
        served.addr,
        "POST",
        &format!("/api/sessions/{}/prompt", idle.id),
        r#"{"prompt":"hello"}"#,
    )
    .await;
    assert_refusal(&response, 503, "draining");

    // A resume provisions a container, so it is refused on the same terms.
    let response = guarded(
        served.addr,
        "POST",
        &format!("/api/sessions/{}/resume", stopped.id),
        "",
    )
    .await;
    assert_refusal(&response, 503, "draining");

    // Reads are what the operator is left with, so they keep working — in the same window.
    let response = fetch(served.addr, "/api/sessions").await;
    assert_eq!(response.status, 200, "{}", response.body);
    for session in [&idle, &running, &stopped] {
        let response = fetch(served.addr, &format!("/api/sessions/{}", session.id)).await;
        assert_eq!(response.status, 200, "{}", response.body);
    }

    // The abort ends the turn the drain is waiting for and the delete removes a session, so both stay open;
    // a drain that refused them would trap exactly the work it is trying to finish.
    let response = guarded(
        served.addr,
        "POST",
        &format!("/api/sessions/{}/abort", running.id),
        "",
    )
    .await;
    assert_eq!(response.status, 202, "{}", response.body);
    let response = guarded(
        served.addr,
        "DELETE",
        &format!("/api/sessions/{}", idle.id),
        "",
    )
    .await;
    assert_eq!(response.status, 202, "{}", response.body);

    // And a steer on the surviving running session is still delivered: guidance into a turn already running
    // starts no new work. The `202` rather than "not 503" is the assertion: `steer` refuses a session that is not
    // `running` (`NOT_RUNNING_STEER`) and answers `500` when the backend cannot take it, and neither of those is
    // "the drain let it through".
    let response = guarded(
        served.addr,
        "POST",
        &format!("/api/sessions/{}/steer", running.id),
        r#"{"guidance":"narrow it"}"#,
    )
    .await;
    assert_eq!(response.status, 202, "{}", response.body);

    served.finish().await.expect("the app stops");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retry_of_an_existing_create_still_answers_its_row_while_draining() {
    let dir = TempDir::new("drain-retry");
    let state = state(&dir);
    let served = Served::start(http::router(state.clone()), GRACE).await;
    // One create while the machine is open, so the key names a row.
    let mut text = String::from("POST /api/sessions HTTP/1.1\r\n");
    for (name, value) in GUARDS {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str(&format!(
        "Idempotency-Key: retry-me\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
        BODY.len()
    ));
    let first = request(served.addr, &text).await;
    assert_eq!(first.status, 202, "{}", first.body);

    state.drain.begin();
    // The same key: the idempotency lookup runs before the machine's refusal (the create's fixed order), so a
    // retry is answered from its row rather than told the machine is busy — and nothing new is created.
    let again = request(served.addr, &text).await;
    assert_eq!(again.status, 202, "{}", again.body);
    assert_eq!(again.body, first.body);
    served.finish().await.expect("the app stops");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_drain_is_not_a_route() {
    let dir = TempDir::new("drain-route");
    let state = state(&dir);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    for (method, path) in [("POST", "/api/drain"), ("GET", "/api/drain")] {
        let response = guarded(served.addr, method, path, "").await;
        assert_refusal(&response, 404, "not_found");
    }
    // No route's path mentions the drain at all, which is the seam's own record of "not reachable over HTTP".
    let app = http::App::new(
        http::router(state.clone()),
        Guards::from_config(&Config::default()),
    );
    assert!(
        !app.manifest()
            .iter()
            .any(|entry| entry.path.contains("drain")),
        "the route table grew a drain: {:?}",
        app.manifest()
    );
    served.finish().await.expect("the app stops");
}
