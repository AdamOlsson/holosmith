//! `POST /api/sessions` as a client meets it (HS-008, ADR-0012).
//!
//! Every test starts the real `App` on an ephemeral port and talks HTTP/1.1 over a socket, so
//! the status, the headers and the bytes are the ones a browser gets. Each test owns its
//! `TempDir` and its own `Database`, so nothing here shares a file with another test (ADR-0009's
//! measured same-process "database is locked" trap).

use std::net::SocketAddr;
use std::time::Duration;

use holosmith::clock;
use holosmith::config::Config;
use holosmith::http::error::{
    BAD_BODY, BAD_CLIENT, BAD_IDEMPOTENCY_KEY, BAD_MODEL, BAD_PROJECT, BAD_THINKING, CAP_REACHED,
    MISSING_IDEMPOTENCY_KEY, UNKNOWN_FIELD,
};
use holosmith::http::{self, AppState};
use holosmith::sessions::{Session, Status};

mod support;
use support::{Response, Served, TempDir, request, state_in};

const GRACE: Duration = Duration::from_secs(10);

/// `Config::default().control_origin`, which is what the default guards expect.
const ORIGIN: &str = "http://localhost:8080";

/// A valid create body: the four fields with values the shape rules accept.
const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

/// Every header a create needs: the three the guards require (ADR-0010) plus the host.
const CREATE_HEADERS: &[(&str, &str)] = &[
    ("Host", "127.0.0.1"),
    ("Origin", ORIGIN),
    ("Content-Type", "application/json"),
    ("X-Holosmith-Client", "1"),
];

/// One create, with no key or exactly one.
async fn post_create(addr: SocketAddr, key: Option<&str>, body: &str) -> Response {
    post_create_with_keys(addr, key.as_slice(), body).await
}

/// One create with as many `Idempotency-Key` headers as `keys` names, so a repeated header is
/// expressible without a test hand-writing a request.
async fn post_create_with_keys(addr: SocketAddr, keys: &[&str], body: &str) -> Response {
    let mut text = String::from("POST /api/sessions HTTP/1.1\r\n");
    for (name, value) in CREATE_HEADERS {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    for key in keys {
        text.push_str(&format!("Idempotency-Key: {key}\r\n"));
    }
    text.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    request(addr, &text).await
}

fn body_with_project(project: &str) -> String {
    format!(r#"{{"project":"{project}","model":"claude-sonnet-4.5","thinking":"high"}}"#)
}

fn body_with_model(model: &str) -> String {
    format!(r#"{{"project":"holosmith","model":"{model}","thinking":"high"}}"#)
}

fn body_with_thinking(thinking: &str) -> String {
    format!(r#"{{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"{thinking}"}}"#)
}

/// The ADR-0008 envelope: the status, `content-type: application/json`, and exactly two keys
/// whose `code` is `code`. A status-only assertion is how a broken contract passes a test.
fn assert_refusal(response: &Response, status: u16, code: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "{}",
        response.body
    );
    let body: serde_json::Value =
        serde_json::from_str(&response.body).expect("an envelope is JSON");
    let object = body.as_object().expect("an envelope is an object");
    assert_eq!(
        object.len(),
        2,
        "the envelope is exactly code and message: {}",
        response.body
    );
    assert_eq!(object["code"], code, "{}", response.body);
    assert!(
        object["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "the envelope carries no message: {}",
        response.body
    );
}

/// A 202 create: the status, the JSON content type, `Location`, and the starting row. Its id and
/// its decoded body are returned so a test can compare them.
fn assert_created(response: &Response) -> (String, serde_json::Value) {
    assert_eq!(response.status, 202, "{}", response.body);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body: serde_json::Value =
        serde_json::from_str(&response.body).expect("a create answers JSON");
    let id = body["id"]
        .as_str()
        .expect("the response names an id")
        .to_string();
    let location = format!("/api/sessions/{id}");
    assert_eq!(response.header("location"), Some(location.as_str()));
    assert_eq!(body["status"].as_str(), Some("starting"));
    (id, body)
}

fn locked_row(state: &AppState, id: &str) -> Session {
    let database = state
        .database
        .lock()
        .expect("the test's database is not poisoned");
    database
        .sessions()
        .get(id)
        .expect("the read succeeds")
        .expect("the created row exists")
}

fn live_count(state: &AppState) -> u64 {
    let database = state
        .database
        .lock()
        .expect("the test's database is not poisoned");
    database
        .sessions()
        .live_count()
        .expect("the live count reads")
}

fn set_status(state: &AppState, id: &str, status: Status) {
    let database = state
        .database
        .lock()
        .expect("the test's database is not poisoned");
    database
        .sessions()
        .set_status(id, status, Some("the test's reason"), clock::unix_now())
        .expect("the update succeeds")
        .expect("the session exists");
}

// ---- the body -----------------------------------------------------------------------

#[tokio::test]
async fn a_non_json_body_is_422_bad_body() {
    let dir = TempDir::new("api-non-json");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let response = post_create(served.addr, Some("non-json"), "not json").await;
    assert_refusal(&response, 422, BAD_BODY);
    assert_eq!(live_count(&state), 0, "a refused body wrote a row");
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_body_of_the_wrong_shape_is_422_bad_body() {
    let dir = TempDir::new("api-wrong-shape");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    for body in [
        "{}",
        r#"{"project":5,"model":"claude-sonnet-4.5","thinking":"high"}"#,
        r#"{"project":"holosmith","thinking":"high"}"#,
        r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":null}"#,
        r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high","network":"yes"}"#,
        "[]",
        "null",
    ] {
        let response = post_create(served.addr, Some("wrong-shape"), body).await;
        assert_refusal(&response, 422, BAD_BODY);
    }
    assert_eq!(live_count(&state), 0);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_body_over_axum_s_two_mebibytes_is_422_bad_body() {
    // The `Bytes` extractor's rejection is mapped to `bad_body` (ADR-0012), so a body axum will
    // not buffer is the ADR-0008 envelope and not axum's own `text/plain` answer.
    let dir = TempDir::new("api-oversized-body");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    // axum's `DefaultBodyLimit` is 2 MiB; this is 64 KiB past it.
    let body = "a".repeat(2 * 1024 * 1024 + 64 * 1024);
    let response = post_create(served.addr, Some("oversized"), &body).await;
    assert_refusal(&response, 422, BAD_BODY);
    assert_eq!(live_count(&state), 0, "a refused body wrote a row");
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn an_unknown_field_is_422_unknown_field() {
    let dir = TempDir::new("api-unknown-field");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    for body in [
        r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high","extra":1}"#,
        r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high","id":"x"}"#,
    ] {
        let response = post_create(served.addr, Some("unknown-field"), body).await;
        assert_refusal(&response, 422, UNKNOWN_FIELD);
    }
    assert_eq!(live_count(&state), 0);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_bad_project_is_422_bad_project() {
    let dir = TempDir::new("api-bad-project");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let too_long = "a".repeat(256);
    for project in ["", ".", "..", "a/b", "a b", too_long.as_str()] {
        let response = post_create(
            served.addr,
            Some("bad-project"),
            &body_with_project(project),
        )
        .await;
        assert_refusal(&response, 422, BAD_PROJECT);
    }
    assert_eq!(live_count(&state), 0);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_bad_model_is_422_bad_model() {
    let dir = TempDir::new("api-bad-model");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let too_long = "a".repeat(129);
    for model in ["", "claude!", too_long.as_str()] {
        let response = post_create(served.addr, Some("bad-model"), &body_with_model(model)).await;
        assert_refusal(&response, 422, BAD_MODEL);
    }
    assert_eq!(live_count(&state), 0);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_bad_thinking_is_422_bad_thinking() {
    let dir = TempDir::new("api-bad-thinking");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let too_long = "a".repeat(33);
    for thinking in ["", "1high", "High", too_long.as_str()] {
        let response = post_create(
            served.addr,
            Some("bad-thinking"),
            &body_with_thinking(thinking),
        )
        .await;
        assert_refusal(&response, 422, BAD_THINKING);
    }
    assert_eq!(live_count(&state), 0);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_shape_rules_accept_their_own_values() {
    // The refusals above are a rule, not a blanket refusal: a dotted, dashed, underscored
    // project, a dotted model and a plain thinking level all create a row.
    let dir = TempDir::new("api-accepted");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let body = r#"{"project":"my.repo_2-x","model":"claude-sonnet-4.5","thinking":"high"}"#;
    let response = post_create(served.addr, Some("accepted"), body).await;
    let (id, _) = assert_created(&response);
    let row = locked_row(&state, &id);
    assert_eq!(row.project, "my.repo_2-x");
    assert_eq!(row.model, "claude-sonnet-4.5");
    assert_eq!(row.thinking, "high");
    served.finish().await.expect("a clean shutdown");
}

// ---- the key ------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_key_is_422_missing_idempotency_key() {
    let dir = TempDir::new("api-missing-key");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let response = post_create(served.addr, None, BODY).await;
    assert_refusal(&response, 422, MISSING_IDEMPOTENCY_KEY);
    assert_eq!(live_count(&state), 0, "a create without a key was written");
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn an_empty_key_is_422_missing_idempotency_key() {
    let dir = TempDir::new("api-empty-key");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let response = post_create(served.addr, Some(""), BODY).await;
    assert_refusal(&response, 422, MISSING_IDEMPOTENCY_KEY);
    assert_eq!(live_count(&state), 0, "an empty key was written");
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn an_over_long_key_is_422_bad_idempotency_key() {
    let dir = TempDir::new("api-long-key");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let key = "k".repeat(201);
    let response = post_create(served.addr, Some(&key), BODY).await;
    assert_refusal(&response, 422, BAD_IDEMPOTENCY_KEY);
    assert_eq!(live_count(&state), 0);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_repeated_key_header_is_422_bad_idempotency_key() {
    let dir = TempDir::new("api-repeated-key");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let response = post_create_with_keys(served.addr, &["one", "two"], BODY).await;
    assert_refusal(&response, 422, BAD_IDEMPOTENCY_KEY);
    assert_eq!(live_count(&state), 0);
    served.finish().await.expect("a clean shutdown");
}

// ---- the row ------------------------------------------------------------------------

#[tokio::test]
async fn a_create_writes_a_starting_row_with_the_key() {
    let dir = TempDir::new("api-create");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let before = clock::unix_now();
    let response = post_create(served.addr, Some("k"), BODY).await;
    let after = clock::unix_now();
    let (id, _) = assert_created(&response);

    let row = locked_row(&state, &id);
    assert_eq!(row.status, Status::Starting);
    assert_eq!(row.client_key.as_deref(), Some("k"));
    assert_eq!(
        row.worktree, "",
        "worktree is '' until the provisioner runs"
    );
    assert_eq!(row.base_commit, None);
    assert_eq!(row.stopped_at, None);
    assert_eq!((row.usage_in, row.usage_out), (0, 0));
    assert_eq!(
        row.created_at, row.last_active_at,
        "a create is the row's first activity"
    );
    assert!(
        (before..=after).contains(&row.created_at),
        "created_at {} is outside [{before}, {after}]",
        row.created_at
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_same_key_answers_the_same_session() {
    let dir = TempDir::new("api-same-key");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let first = post_create(served.addr, Some("same"), BODY).await;
    let (first_id, first_body) = assert_created(&first);
    let second = post_create(served.addr, Some("same"), BODY).await;
    let (second_id, second_body) = assert_created(&second);
    assert_eq!(second_id, first_id, "a retry created a second session");
    assert_eq!(
        second_body, first_body,
        "a retry did not answer the same row"
    );
    assert_eq!(live_count(&state), 1, "a retry wrote a second row");
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_different_body_with_the_same_key_is_the_same_session() {
    // The key is the identity, not the body: a retry cannot tell whether its first request
    // arrived, so it must answer the first session whatever the second body says.
    let dir = TempDir::new("api-key-identity");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let first = post_create(served.addr, Some("identity"), BODY).await;
    let (first_id, _) = assert_created(&first);
    let second = post_create(
        served.addr,
        Some("identity"),
        &body_with_project("another-project"),
    )
    .await;
    let (second_id, second_body) = assert_created(&second);
    assert_eq!(second_id, first_id);
    assert_eq!(second_body["project"].as_str(), Some("holosmith"));
    assert_eq!(live_count(&state), 1);
    served.finish().await.expect("a clean shutdown");
}

// ---- the cap ------------------------------------------------------------------------

fn one_live_session() -> Config {
    Config {
        max_live_sessions: 1,
        ..Config::default()
    }
}

#[tokio::test]
async fn a_full_machine_is_409_cap_reached() {
    let dir = TempDir::new("api-cap");
    let state = state_in(&dir, one_live_session());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    assert_created(&post_create(served.addr, Some("filler"), BODY).await);
    let refused = post_create(served.addr, Some("fresh"), BODY).await;
    assert_refusal(&refused, 409, CAP_REACHED);
    assert_eq!(
        live_count(&state),
        1,
        "the refusal evicted or wrote something"
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_retry_of_the_session_that_filled_the_cap_still_answers_202() {
    // The lookup runs before the cap count: a machine at the cap has not made the client's own
    // retry invalid.
    let dir = TempDir::new("api-cap-retry");
    let state = state_in(&dir, one_live_session());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let (first_id, _) = assert_created(&post_create(served.addr, Some("filler"), BODY).await);
    let retry = post_create(served.addr, Some("filler"), BODY).await;
    let (retry_id, _) = assert_created(&retry);
    assert_eq!(retry_id, first_id);
    assert_refusal(
        &post_create(served.addr, Some("fresh"), BODY).await,
        409,
        CAP_REACHED,
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn stopped_sessions_do_not_count_towards_the_cap() {
    let dir = TempDir::new("api-stopped");
    let state = state_in(&dir, one_live_session());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let (id, _) = assert_created(&post_create(served.addr, Some("stopped"), BODY).await);
    set_status(&state, &id, Status::Stopped);
    let (fresh_id, _) = assert_created(&post_create(served.addr, Some("fresh"), BODY).await);
    assert_ne!(fresh_id, id);
    assert_eq!(live_count(&state), 1);
    served.finish().await.expect("a clean shutdown");
}

// ---- the answer ---------------------------------------------------------------------

#[tokio::test]
async fn a_create_answers_202_with_the_row_and_one_extra_key() {
    let dir = TempDir::new("api-response");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let response = post_create(served.addr, Some("response"), BODY).await;
    let (id, body) = assert_created(&response);
    let row = locked_row(&state, &id);
    let object = body.as_object().expect("the response is an object");
    assert_eq!(
        object.len(),
        16,
        "the response must be the fifteen row columns plus events: {}",
        response.body
    );
    assert_eq!(body["id"].as_str(), Some(row.id.as_str()));
    assert_eq!(body["project"].as_str(), Some(row.project.as_str()));
    assert_eq!(body["model"].as_str(), Some(row.model.as_str()));
    assert_eq!(body["thinking"].as_str(), Some(row.thinking.as_str()));
    assert_eq!(body["worktree"].as_str(), Some(row.worktree.as_str()));
    assert_eq!(body["base_commit"].as_str(), row.base_commit.as_deref());
    assert_eq!(body["status"].as_str(), Some(row.status.as_str()));
    assert_eq!(body["status_reason"].as_str(), row.status_reason.as_deref());
    assert_eq!(body["client_key"].as_str(), row.client_key.as_deref());
    assert_eq!(body["network"].as_bool(), Some(row.network));
    assert_eq!(body["created_at"].as_i64(), Some(row.created_at));
    assert_eq!(body["last_active_at"].as_i64(), Some(row.last_active_at));
    assert_eq!(body["stopped_at"].as_i64(), row.stopped_at);
    assert_eq!(body["usage_in"].as_u64(), Some(row.usage_in));
    assert_eq!(body["usage_out"].as_u64(), Some(row.usage_out));
    assert_eq!(
        body["events"].as_str(),
        Some(format!("/api/sessions/{id}/events").as_str())
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_create_starts_no_provisioning() {
    let dir = TempDir::new("api-no-provisioning");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let response = post_create(served.addr, Some("no-provisioning"), BODY).await;
    let (id, body) = assert_created(&response);
    assert_eq!(body["status"].as_str(), Some("starting"));
    let row = locked_row(&state, &id);
    assert_eq!(row.status, Status::Starting);
    assert_eq!(row.worktree, "");
    assert_eq!(row.base_commit, None);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let later = locked_row(&state, &id);
    assert_eq!(later, row, "the handler changed the row after it answered");
    served.finish().await.expect("a clean shutdown");
}

// ---- the guards ---------------------------------------------------------------------

#[tokio::test]
async fn the_new_route_is_behind_the_guards() {
    // The route is a non-`GET`, so ADR-0010's three headers already apply: without the client
    // header nothing reaches the handler and no row exists.
    let dir = TempDir::new("api-guards");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let text = format!(
        "POST /api/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
        BODY.len()
    );
    let response = request(served.addr, &text).await;
    assert_refusal(&response, 403, BAD_CLIENT);
    assert_eq!(live_count(&state), 0);
    served.finish().await.expect("a clean shutdown");
}
