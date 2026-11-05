//! `GET /api/sessions/:id/tool-output/:tool_id` and the file behind it (HS-033, spec item 15).
//!
//! The store's own rules — the cap, the marker, the id allow-list — are unit tests in `tool.rs`; what only a
//! socket can answer is the *endpoint*: that it serves the bytes, that it answers the envelope for a session
//! the table does not hold, an id that cannot be a filename and a file that is not there, and that no spelling
//! of a traversal reaches a file outside `<data>/sessions/<id>/tool/`.
//!
//! Nothing here boots Docker or a real agent: the file is written by the same [`Store`] the pump uses, and the
//! turn that produced it in a real container is `tests/rpc.rs`'s recorded fixture.

use std::time::Duration;

use holosmith::clock;
use holosmith::config::Config;
use holosmith::http;
use holosmith::sessions::{NewSession, Session};
use holosmith::tool::{Store, truncated_marker};

mod support;
use support::{Served, TempDir, fetch};

/// The drain deadline each test gives `serve`, so a socket a test forgot to drop cannot hold it open.
const GRACE: Duration = Duration::from_millis(100);

/// A session on `state`'s database, so the endpoint has a row to look up.
fn create_session(state: &holosmith::http::AppState, client_key: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .create(
            &NewSession {
                project: "holosmith".to_string(),
                model: "claude-sonnet-4.5".to_string(),
                thinking: "high".to_string(),
                network: false,
                client_key: Some(client_key.to_string()),
                base_commit: None,
            },
            clock::unix_now(),
        )
        .expect("a session is created")
}

/// The default config with the data root the test's own directory, because the endpoint reads
/// `<data>/sessions/<id>/tool/<tool_id>.txt` and the store writes it. Nothing else is changed.
fn config(dir: &TempDir) -> Config {
    Config {
        data_root: dir.path().to_path_buf(),
        ..Config::default()
    }
}

/// The one file name a test uses when the id is not the point.
const TOOL: &str = "call_00_9hlt8we6459oa420spsp3mju";

#[tokio::test]
async fn the_endpoint_serves_the_file_the_pump_wrote() {
    let dir = TempDir::new("tool-output-serve");
    let state = support::state_in(&dir, config(&dir));
    let session = create_session(&state, "tool-serve");
    // Written through the store rather than by hand: the path the endpoint reads is the path the pump writes,
    // and a test that spelled the path itself could not notice the two drifting apart.
    let store = Store::new(dir.path());
    let printed = "hello from the tool\n";
    store
        .store(&session.id, TOOL, printed)
        .expect("the output is stored");

    let served = Served::start(http::router(state.clone()), GRACE).await;
    let answer = fetch(
        served.addr,
        &format!("/api/sessions/{}/tool-output/{TOOL}", session.id),
    )
    .await;
    assert_eq!(answer.status, 200, "{answer:?}", answer = answer.body);
    assert_eq!(
        answer.header("content-type"),
        Some("text/plain; charset=utf-8"),
        "a tool's output is its own text, not a JSON envelope"
    );
    assert_eq!(answer.body, printed);
    served.finish().await.expect("clean shutdown");
}

#[tokio::test]
async fn a_capped_file_is_served_with_the_marker_the_reader_needs() {
    // The endpoint does not cap anything: the file is what the store made of a large output, and what a client
    // fetches is that file — marker included, because a reader that cannot see where the text stopped would
    // believe it had all of it.
    let dir = TempDir::new("tool-output-capped");
    let state = support::state_in(&dir, config(&dir));
    let session = create_session(&state, "tool-capped");
    let store = Store::new(dir.path());
    let printed = "x".repeat(holosmith::tool::CAP + 1024);
    store
        .store(&session.id, TOOL, &printed)
        .expect("the output is stored");

    let served = Served::start(http::router(state.clone()), GRACE).await;
    let answer = fetch(
        served.addr,
        &format!("/api/sessions/{}/tool-output/{TOOL}", session.id),
    )
    .await;
    assert_eq!(answer.status, 200);
    // The body is the cap's worth of output, a newline, the marker and its newline: no more.
    let marker = truncated_marker(holosmith::tool::CAP, printed.len());
    assert_eq!(
        answer.body.len(),
        holosmith::tool::CAP + 1 + marker.len() + 1,
        "the served file is not the capped body plus one marker line"
    );
    assert!(
        answer.body.trim_end().ends_with(&marker),
        "the served file does not name where it stopped"
    );
    served.finish().await.expect("clean shutdown");
}

#[tokio::test]
async fn an_id_that_cannot_be_a_filename_is_not_a_file_to_read() {
    let dir = TempDir::new("tool-output-traversal");
    let state = support::state_in(&dir, config(&dir));
    let session = create_session(&state, "tool-traversal");
    let store = Store::new(dir.path());
    store
        .store(&session.id, TOOL, "the real output")
        .expect("the output is stored");
    // The canary sits where a naive join would land. The stored name is `<id>.txt`, so the id `..%2Fsecret`
    // becomes `<data>/sessions/<id>/tool/../secret.txt`: one level up. (Axum decodes a `%2F` inside a segment —
    // measured — so the handler really does see `../secret`; a canary at `<data>/secret.txt` would be two levels
    // away and every assertion below would pass with the id check deleted, which is what this story's review
    // caught.)
    let session_dir = dir.path().join("sessions").join(&session.id);
    std::fs::write(session_dir.join("secret.txt"), "not yours").expect("a file to reach for");

    let served = Served::start(http::router(state.clone()), GRACE).await;
    // `..` cannot be spelled with the id rule's characters, so each attempt lands on "no such output" rather
    // than on a path. The encoded forms are here because axum decodes the segment before the handler sees it:
    // `%2e%2e%2f` arrives as `../`.
    for id in [
        "..",
        "..%2Fsecret",
        "%2e%2e%2fsecret",
        "..%2F..%2F..%2Fapp",
        "a/b",
        "a%2Fb",
        ".hidden",
        "with%20space",
        "%2e%2e",
        "x".repeat(129).as_str(),
    ] {
        let answer = fetch(
            served.addr,
            &format!("/api/sessions/{}/tool-output/{id}", session.id),
        )
        .await;
        assert_eq!(answer.status, 404, "{id}: {}", answer.body);
        assert!(
            answer.body.contains("not_found"),
            "{id}: the envelope's code is missing: {}",
            answer.body
        );
        assert!(
            !answer.body.contains("not yours"),
            "{id}: a traversal reached a file outside the session's tool directory: {}",
            answer.body
        );
    }
    served.finish().await.expect("clean shutdown");
}

#[tokio::test]
async fn a_session_the_table_does_not_hold_and_a_file_that_is_not_there_are_the_same_answer() {
    let dir = TempDir::new("tool-output-missing");
    let state = support::state_in(&dir, config(&dir));
    let session = create_session(&state, "tool-missing");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    // A session nobody created: the row is checked before any directory is read.
    let unknown = fetch(
        served.addr,
        &format!("/api/sessions/0123456789abcdef/tool-output/{TOOL}"),
    )
    .await;
    assert_eq!(unknown.status, 404, "{}", unknown.body);
    assert!(unknown.body.contains("not_found"), "{}", unknown.body);

    // A session that exists with no such output.
    let missing = fetch(
        served.addr,
        &format!("/api/sessions/{}/tool-output/{TOOL}", session.id),
    )
    .await;
    assert_eq!(missing.status, 404, "{}", missing.body);
    assert!(missing.body.contains("not_found"), "{}", missing.body);

    // And the two sentences differ, because the requests differ (ADR-0008: the code is stable, the sentence
    // describes its own request).
    assert_ne!(unknown.body, missing.body);
    served.finish().await.expect("clean shutdown");
}

#[tokio::test]
async fn a_deleted_sessions_output_is_not_reachable() {
    // The row is the gate as well as the key: a directory a delete left behind (HS-043 owns what survives it)
    // must not be readable through a session nobody can see.
    let dir = TempDir::new("tool-output-deleted");
    let state = support::state_in(&dir, config(&dir));
    let session = create_session(&state, "tool-deleted");
    let store = Store::new(dir.path());
    store
        .store(&session.id, TOOL, "still on disk")
        .expect("the output is stored");
    {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .sessions()
            .remove(&session.id)
            .expect("the row is removed");
    }
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let answer = fetch(
        served.addr,
        &format!("/api/sessions/{}/tool-output/{TOOL}", session.id),
    )
    .await;
    assert_eq!(answer.status, 404, "{}", answer.body);
    assert!(
        !answer.body.contains("still on disk"),
        "a deleted session's tool output was served: {}",
        answer.body
    );
    served.finish().await.expect("clean shutdown");
}
