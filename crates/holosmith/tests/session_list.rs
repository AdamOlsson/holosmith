//! `GET /api/sessions` as a client meets it (HS-013, ADR-0017).
//!
//! Every test starts the real `App` on an ephemeral port and talks HTTP/1.1 over a socket, so the
//! status, the headers and the bytes are the ones a browser gets. Each test owns its `TempDir` and
//! its own `Database`, so nothing here shares a file with another test (ADR-0009's measured
//! same-process "database is locked" trap).

use std::time::Duration;

use axum::http::Method;
use holosmith::config::Config;
use holosmith::http::error::BAD_HOST;
use holosmith::http::{self, Guards, RouteKind};

mod support;
use support::{Response, Served, TempDir, fetch, request, state, state_in};

const GRACE: Duration = Duration::from_secs(10);

/// `Config::default().control_origin`, which is what the default guards expect.
const ORIGIN: &str = "http://localhost:8080";

/// A valid create body, so a test can put one row in the table over the socket.
const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

/// The row's fifteen column names, in column order. A list element is the row itself, so a missing
/// name here is a client that cannot render a session.
const COLUMNS: [&str; 15] = [
    "id",
    "project",
    "model",
    "thinking",
    "worktree",
    "base_commit",
    "status",
    "status_reason",
    "client_key",
    "network",
    "created_at",
    "last_active_at",
    "stopped_at",
    "usage_in",
    "usage_out",
];

/// One create with the headers the guards require (ADR-0010), answering the row.
async fn create(addr: std::net::SocketAddr, key: &str) -> Response {
    let text = format!(
        "POST /api/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\n\
         Idempotency-Key: {key}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
        BODY.len()
    );
    request(addr, &text).await
}

/// The list's body as JSON, asserting the status and the content type first: a status-only
/// assertion is how a wrong contract passes a test.
fn listed(response: &Response) -> serde_json::Value {
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "{}",
        response.body
    );
    serde_json::from_str(&response.body).expect("the list answers JSON")
}

/// A default config with the machine's network default turned on, which is the one setting the
/// list carries.
fn network_on() -> Config {
    Config {
        network_default: true,
        ..Config::default()
    }
}

#[tokio::test]
async fn an_empty_machine_answers_no_sessions_and_the_machines_default() {
    let dir = TempDir::new("session-list-empty");
    let served = Served::start(http::router(state_in(&dir, Config::default())), GRACE).await;
    let response = fetch(served.addr, "/api/sessions").await;
    let body = listed(&response);
    assert_eq!(
        response.body, r#"{"sessions":[],"network_default":false,"stopped_ttl_days":7}"#,
        "the empty list is one object with three keys"
    );
    assert_eq!(body["sessions"].as_array().map(Vec::len), Some(0));
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_created_session_is_listed_with_every_column_and_still_starting() {
    let dir = TempDir::new("session-list-one");
    let served = Served::start(http::router(state_in(&dir, Config::default())), GRACE).await;
    let created = create(served.addr, "list-key-1").await;
    assert_eq!(created.status, 202, "{}", created.body);
    let created: serde_json::Value =
        serde_json::from_str(&created.body).expect("a create answers JSON");
    let id = created["id"]
        .as_str()
        .expect("the create names an id")
        .to_string();

    let response = fetch(served.addr, "/api/sessions").await;
    let body = listed(&response);
    let sessions = body["sessions"]
        .as_array()
        .expect("the list carries a sessions array");
    assert_eq!(sessions.len(), 1, "{}", response.body);
    let row = sessions[0]
        .as_object()
        .expect("a list element is a JSON object");
    assert_eq!(
        row.len(),
        COLUMNS.len(),
        "the element is the row's fifteen columns: {}",
        response.body
    );
    for name in COLUMNS {
        assert!(row.contains_key(name), "{name} is missing from {row:?}");
    }
    assert_eq!(row["id"].as_str(), Some(id.as_str()));
    assert_eq!(row["project"].as_str(), Some("holosmith"));
    // The harness's stub takes a minute per step, so the row is still `starting`: the state the
    // create answered with, which is what the screen renders before any stream frame arrives.
    assert_eq!(row["status"].as_str(), Some("starting"));
    assert_eq!(body["network_default"].as_bool(), Some(false));
    // HS-064: the machine's stopped-session window, beside the machine's other setting. The default is the
    // spec's seven days (item 15), and it is a number rather than a sentence because the page draws the
    // countdown from it and the row's own `stopped_at`.
    assert_eq!(body["stopped_ttl_days"].as_u64(), Some(7));
    // A list element is a `Session` and nothing else: only the create's answer adds `events`.
    assert!(!row.contains_key("events"), "{}", response.body);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_lists_network_default_is_the_configs() {
    let dir = TempDir::new("session-list-network");
    let served = Served::start(http::router(state_in(&dir, network_on())), GRACE).await;
    let response = fetch(served.addr, "/api/sessions").await;
    let body = listed(&response);
    assert_eq!(
        body["network_default"].as_bool(),
        Some(true),
        "the checkbox default is the machine's: {}",
        response.body
    );
    assert_eq!(body["sessions"].as_array().map(Vec::len), Some(0));
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_lists_retention_window_is_the_configs() {
    // HS-064's number is the machine's and not the renderer's: a machine that keeps stopped sessions for a
    // fortnight says so, and the page's countdown follows. A second config, because a test that only ever read
    // the default could pass over a hard-coded `7`.
    let dir = TempDir::new("session-list-ttl");
    let config = Config {
        stopped_ttl_days: 14,
        ..Config::default()
    };
    let served = Served::start(http::router(state_in(&dir, config)), GRACE).await;
    let response = fetch(served.addr, "/api/sessions").await;
    let body = listed(&response);
    assert_eq!(
        body["stopped_ttl_days"].as_u64(),
        Some(14),
        "the window is the machine's: {}",
        response.body
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_foreign_host_is_refused_before_the_list_is_read() {
    let dir = TempDir::new("session-list-guards");
    let served = Served::start(http::router(state_in(&dir, Config::default())), GRACE).await;
    let response = request(
        served.addr,
        "GET /api/sessions HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(response.status, 403, "{}", response.body);
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "{}",
        response.body
    );
    let body: serde_json::Value =
        serde_json::from_str(&response.body).expect("an envelope is JSON");
    assert_eq!(body["code"], BAD_HOST);
    served.finish().await.expect("a clean shutdown");
}

#[test]
fn the_list_route_is_recorded_read_only() {
    // "Read-only" is not observable from outside, so it is read from the manifest `Routes::get`
    // fills: a list registered as mutating would demand an `Origin` and a key it has no use for.
    let app = http::App::new(
        http::router(state()),
        Guards::from_config(&Config::default()),
    );
    let entry = app
        .manifest()
        .iter()
        .find(|entry| entry.path == "/api/sessions" && entry.method == Method::GET)
        .expect("the list is a registered route");
    assert_eq!(entry.kind, RouteKind::ReadOnly);
    // The create on the same path is still the mutating one: two methods, two kinds.
    let post = app
        .manifest()
        .iter()
        .find(|entry| entry.path == "/api/sessions" && entry.method == Method::POST)
        .expect("the create is a registered route");
    assert_eq!(post.kind, RouteKind::Mutating);
}
