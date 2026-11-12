//! One session by id, and the screen that opens with it.
//!
//! Every test starts the real `App` on an ephemeral port and talks HTTP/1.1 over a socket, so the
//! status, the headers and the bytes are the ones a browser gets. Each test owns its `TempDir`, so
//! nothing here shares a database file with another test (ADR-0009's same-process "database is
//! locked" trap).

use std::time::Duration;

use axum::http::Method;
use holosmith::config::Config;
use holosmith::http::error::{BAD_HOST, NOT_FOUND};
use holosmith::http::{self, Guards, RouteKind};

mod support;
use support::{Response, Served, TempDir, fetch, request, state, state_in};

const GRACE: Duration = Duration::from_secs(10);

/// `Config::default().control_origin`, which is what the default guards expect.
const ORIGIN: &str = "http://localhost:8080";

/// A valid create body, so a test can put one row in the table over the socket.
const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

/// The row's fifteen column names, in column order. The one row is the list's element, so a missing
/// name here is a session screen that cannot render its session.
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

/// A response that must be the ADR-0008 envelope for `code`: status, `content-type` and code, all
/// three, because a status-only assertion is how the wrong code ships.
fn assert_envelope(response: &Response, status: u16, code: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "{}",
        response.body
    );
    let body: serde_json::Value =
        serde_json::from_str(&response.body).expect("an envelope is JSON");
    assert_eq!(body["code"], code, "{}", response.body);
}

#[tokio::test]
async fn a_created_session_is_answered_by_id_with_every_column() {
    let dir = TempDir::new("session-screen-one");
    let served = Served::start(http::router(state_in(&dir, Config::default())), GRACE).await;
    let created = create(served.addr, "screen-key-1").await;
    assert_eq!(created.status, 202, "{}", created.body);
    let created: serde_json::Value =
        serde_json::from_str(&created.body).expect("a create answers JSON");
    let id = created["id"]
        .as_str()
        .expect("the create names an id")
        .to_string();

    let response = fetch(served.addr, &format!("/api/sessions/{id}")).await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "{}",
        response.body
    );
    let row: serde_json::Value = serde_json::from_str(&response.body).expect("the answer is JSON");
    let row = row.as_object().expect("the answer is one object");
    assert_eq!(
        row.len(),
        COLUMNS.len(),
        "the answer is the row's fifteen columns: {}",
        response.body
    );
    for name in COLUMNS {
        assert!(row.contains_key(name), "{name} is missing from {row:?}");
    }
    assert_eq!(row["id"].as_str(), Some(id.as_str()));
    assert_eq!(row["project"].as_str(), Some("holosmith"));
    assert_eq!(row["thinking"].as_str(), Some("high"));
    assert_eq!(row["network"].as_bool(), Some(false));
    // One row is the row: only the create's answer adds `events`, and only the list adds
    // `network_default` (ADR-0017).
    assert!(!row.contains_key("events"), "{}", response.body);
    assert!(!row.contains_key("network_default"), "{}", response.body);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn an_unknown_session_id_is_not_found() {
    let dir = TempDir::new("session-screen-missing");
    let served = Served::start(http::router(state_in(&dir, Config::default())), GRACE).await;
    let response = fetch(served.addr, "/api/sessions/0000000000000000").await;
    assert_envelope(&response, 404, NOT_FOUND);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_foreign_host_is_refused_before_the_row_is_read() {
    let dir = TempDir::new("session-screen-guards");
    let served = Served::start(http::router(state_in(&dir, Config::default())), GRACE).await;
    let created = create(served.addr, "screen-key-2").await;
    let id = serde_json::from_str::<serde_json::Value>(&created.body)
        .expect("a create answers JSON")["id"]
        .as_str()
        .expect("the create names an id")
        .to_string();
    let response = request(
        served.addr,
        &format!(
            "GET /api/sessions/{id} HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n"
        ),
    )
    .await;
    assert_envelope(&response, 403, BAD_HOST);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_screen_path_is_served_as_the_app_shell() {
    // `/sessions/<id>` is not a route and not a file the app ships, so the fallback answers it with
    // the shell (ADR-0007). The page reads its id from the address, which is what makes a reload of
    // one session's screen work with no router installed (ADR-0018).
    let dir = TempDir::new("session-screen-shell");
    let served = Served::start(http::router(state_in(&dir, Config::default())), GRACE).await;
    let shell = fetch(served.addr, "/").await;
    assert_eq!(shell.status, 200, "{}", shell.body);
    let screen = fetch(served.addr, "/sessions/0123456789abcdef").await;
    assert_eq!(screen.status, 200, "{}", screen.body);
    assert_eq!(
        screen.header("content-type"),
        Some("text/html; charset=utf-8"),
        "the screen path is a page, not an envelope"
    );
    assert_eq!(
        screen.body, shell.body,
        "the screen path is the same shell the app serves at /"
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_segment_that_is_not_utf8_is_the_envelope_not_a_plain_text_400() {
    // A `Path<String>` rejection is axum's `text/plain` 400, which would be the one answer on this
    // prefix outside ADR-0008. A segment that cannot name a row is a row the table does not hold.
    let dir = TempDir::new("session-screen-utf8");
    let served = Served::start(http::router(state_in(&dir, Config::default())), GRACE).await;
    for path in ["/api/sessions/%FF", "/api/sessions/%FF/events"] {
        let response = fetch(served.addr, path).await;
        assert_envelope(&response, 404, NOT_FOUND);
    }
    served.finish().await.expect("a clean shutdown");
}

#[test]
fn the_one_route_is_recorded_read_only() {
    // Not observable from outside, so it is read from the manifest `Routes::get` fills: a row
    // registered as mutating would demand an `Origin` and a key it has no use for.
    let app = http::App::new(
        http::router(state()),
        Guards::from_config(&Config::default()),
    );
    let entry = app
        .manifest()
        .iter()
        .find(|entry| entry.path == "/api/sessions/{id}" && entry.method == Method::GET)
        .expect("the one row is a registered route");
    assert_eq!(entry.kind, RouteKind::ReadOnly);
}
