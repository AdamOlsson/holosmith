//! The hub's forwarding, as a client meets it (spec item 3, ADR-0074).
//!
//! Four groups, one per thing the story had to get right:
//!
//! - **The forwarded call** over the wire: one peer written by a stand-in, the request this machine sends it (its
//!   own origin in `Host` and `Origin`, the caller's `Idempotency-Key` and `Last-Event-ID` verbatim), and the peer's
//!   answer relayed with its status, its headers and its bytes.
//! - **The three decisions before any dial**: `local` is a `308` to `/api/<rest>`, a name the registry does not hold
//!   is `404 unknown_host`, the registry itself is `403 not_forwarded`, and a machine whose role is `server` has no
//!   such route at all.
//! - **The failures a client can tell apart**: a peer that is gone is `502 upstream_unreachable`, a peer that takes
//!   the request and never answers is `504 upstream_timeout` — measured against a shortened bound, because waiting
//!   five seconds to prove a constant is a slow test of a fast rule.
//! - **The preview hop**, which needs no daemon: a stand-in peer's *preview listener* receives the request rewritten
//!   into the peer's own spelling (`/local/<id>/…`) with the peer's preview origin in `Host` and `Origin`, and an
//!   upgrade's `Origin` is the peer's rather than the hub's. The four refusals are here too.
//!
//! What is **not** here, and is the gate's `case_forward`: two real `serve` processes, the probe that learns a peer's
//! preview origin from its health answer, a session created through the hub, an event stream relayed byte for byte,
//! a peer killed mid-stream, and a real container's dev server behind an upgrade.

use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use holosmith::config::{Config, Origin, Role};
use holosmith::forward::Forwarding;
use holosmith::hosts::{Host, Reach};
use holosmith::http::error::{
    NOT_FORWARDED, NOT_FOUND, SESSIONS_DISABLED, UNKNOWN_HOST, UPSTREAM_TIMEOUT,
    UPSTREAM_UNREACHABLE,
};
use holosmith::http::{self, AppState};
use holosmith::preflight::Tools;
use holosmith::preview;

mod support;
use support::{Response, Served, TempDir, fast_stub, request, state_in, with_stub};

const GRACE: Duration = Duration::from_secs(10);

/// The default config's control origin, which is what the default guards expect of a mutating request.
const ORIGIN: &str = "http://localhost:8080";

/// The name a peer is registered under in every test below.
const PEER: &str = "workshop";

/// One session id, in the one alphabet this build has.
const ID: &str = "0123456789abcdef";

/// A stand-in peer: a socket that answers a request with whatever the test says, and remembers what it was asked.
struct Peer {
    address: SocketAddr,
    seen: Arc<Mutex<Vec<String>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl Peer {
    /// A peer that reads one request and answers `reply(head)`, for as long as the test runs.
    ///
    /// A **tokio** task rather than a blocking one: `spawn_blocking` cannot be aborted, so a stand-in that loops on
    /// `accept` would keep the runtime's shutdown waiting for a connection that never comes (the landmine
    /// `hosts::probe`'s test recorded the other way round). `abort` is what `Drop` calls.
    async fn start<F>(reply: F) -> Self
    where
        F: Fn(&str) -> Option<Vec<u8>> + Send + Sync + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is bindable");
        let address = listener.local_addr().expect("a bound address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let Some(head) = read_head(&mut stream).await else {
                    continue;
                };
                log.lock()
                    .expect("the log is not poisoned")
                    .push(head.clone());
                match reply(&head) {
                    Some(answer) => {
                        let _ = stream.write_all(&answer).await;
                        let _ = stream.flush().await;
                        let _ = stream.shutdown().await;
                    }
                    // A peer that says nothing **holds** the connection: closing it would be an answer, and the
                    // timeout arm is about a socket that is open and silent.
                    None => {
                        tokio::spawn(async move {
                            let _held = stream;
                            std::future::pending::<()>().await;
                        });
                    }
                }
            }
        });
        Self {
            address,
            seen,
            handle,
        }
    }

    /// A peer that answers every request with one fixed HTTP reply.
    async fn answering(answer: &'static str) -> Self {
        Self::start(move |_| Some(answer.as_bytes().to_vec())).await
    }

    fn origin(&self) -> String {
        format!("http://{}", self.address)
    }

    /// The one request the peer was asked, as the bytes it arrived as.
    fn asked(&self) -> String {
        self.seen
            .lock()
            .expect("the log is not poisoned")
            .first()
            .cloned()
            .expect("the peer was asked nothing")
    }

    fn asked_count(&self) -> usize {
        self.seen.lock().expect("the log is not poisoned").len()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Read one request head, splitting `head + body` so a body is consumed before the peer answers.
async fn read_head(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await
        {
            Ok(Ok(read)) => read,
            _ => return None,
        };
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = buffer
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|at| at + 4)
        {
            // The body, if the head declared one: read it so the peer's answer is not written into the middle of a
            // request the client is still sending.
            let head = String::from_utf8_lossy(&buffer[..end]).to_string();
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            while buffer.len() < end + length {
                match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await {
                    Ok(Ok(0)) | Err(_) => break,
                    Ok(Ok(read)) => buffer.extend_from_slice(&chunk[..read]),
                    Ok(Err(_)) => break,
                }
            }
            return Some(head);
        }
    }
}

/// A hub: a state whose registry holds `rows`, serving the control listener's routes.
async fn hub(dir: &TempDir, config: Config, rows: &[Host]) -> (Served, AppState) {
    let state = with_stub(state_in(dir, config), fast_stub());
    for row in rows {
        state
            .database
            .lock()
            .expect("the lock is not poisoned")
            .hosts()
            .upsert(row)
            .expect("the row is stored");
    }
    let served = Served::start(http::router(state.clone()), GRACE).await;
    (served, state)
}

/// A body with its chunk framing removed, so a test can compare what a client would *see*.
///
/// `support::parse` keeps the bytes as they arrived (the harness reads a stream's raw answer), and hyper frames a
/// length-less answer as chunked — so a comparison of event bytes has to decode first, or it compares framing.
fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::new();
    let mut rest = body;
    while let Some(end) = rest.windows(2).position(|window| window == b"\r\n") {
        let Ok(line) = std::str::from_utf8(&rest[..end]) else {
            break;
        };
        let Ok(size) = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
        else {
            break;
        };
        let data = &rest[end + 2..];
        let take = size.min(data.len());
        decoded.extend_from_slice(&data[..take]);
        if size == 0 || take < size || rest.len() < end + 2 + size + 2 {
            break;
        }
        rest = &rest[end + 2 + size + 2..];
    }
    decoded
}

fn row(name: &str, origin: &str, preview: Option<&str>) -> Host {
    Host {
        name: name.to_string(),
        url: Origin::parse(origin).expect("a test origin"),
        preview: preview.map(|preview| Origin::parse(preview).expect("a test preview origin")),
        reach: Reach::Reachable,
        version: Some("0.1.0".to_string()),
        last_error: None,
        last_checked_at: 1,
    }
}

/// One request with the four headers a mutating call needs (ADR-0010).
async fn send(
    addr: SocketAddr,
    method: &str,
    path: &str,
    extra: &[(&str, &str)],
    body: &str,
) -> Response {
    let mut text = format!("{method} {path} HTTP/1.1\r\n");
    text.push_str("Host: 127.0.0.1\r\n");
    text.push_str(&format!("Origin: {ORIGIN}\r\n"));
    text.push_str("Content-Type: application/json\r\n");
    text.push_str("X-Holosmith-Client: 1\r\n");
    for (name, value) in extra {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    request(addr, &text).await
}

async fn get(addr: SocketAddr, path: &str) -> Response {
    request(
        addr,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
    .await
}

/// The ADR-0008 envelope: the status, JSON, and exactly the code the request earned.
fn assert_refusal(response: &Response, status: u16, code: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    let value: serde_json::Value = serde_json::from_str(&response.body).expect("a refusal is JSON");
    assert_eq!(value["code"], code, "{}", response.body);
}

// ---- the forwarded call -----------------------------------------------------------------------------

#[tokio::test]
async fn a_forwarded_call_is_the_peers_answer_byte_for_byte() {
    let dir = TempDir::new("forward-relay");
    let peer = Peer::answering(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 38\r\n\
         connection: close\r\n\r\n{\"sessions\":[],\"live\":0,\"cap\":3,\"x\":1}",
    )
    .await;
    let (served, _state) = hub(
        &dir,
        Config::default(),
        &[row(PEER, &peer.origin(), Some("http://127.0.0.1:1"))],
    )
    .await;
    let answer = get(served.addr, &format!("/api/hosts/{PEER}/sessions")).await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(
        answer.body, "{\"sessions\":[],\"live\":0,\"cap\":3,\"x\":1}",
        "the peer's body is the answer"
    );
    assert_eq!(answer.header("content-type"), Some("application/json"));
    assert_eq!(
        answer.header("content-length"),
        Some("38"),
        "a bounded answer keeps the peer's own length"
    );
    // And what the peer was asked: this machine's path prefix removed, the peer's own origin in both headers, and
    // no connection the peer has to keep.
    let asked = peer.asked();
    assert!(
        asked.starts_with("GET /api/sessions HTTP/1.1\r\n"),
        "{asked}"
    );
    assert!(
        asked.contains(&format!("Host: {}\r\n", peer.address)),
        "{asked}"
    );
    assert!(
        asked.contains(&format!("Origin: {}\r\n", peer.origin())),
        "{asked}"
    );
    assert!(asked.contains("Connection: close\r\n"), "{asked}");
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_callers_idempotency_key_and_resume_point_survive_the_hop() {
    let dir = TempDir::new("forward-headers");
    let peer = Peer::answering(
        "HTTP/1.1 202 Accepted\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\
         connection: close\r\n\r\n{}",
    )
    .await;
    let (served, _state) = hub(
        &dir,
        Config::default(),
        &[row(PEER, &peer.origin(), Some("http://127.0.0.1:1"))],
    )
    .await;
    let answer = send(
        served.addr,
        "POST",
        &format!("/api/hosts/{PEER}/sessions"),
        &[("Idempotency-Key", "retry-me"), ("Last-Event-ID", "41")],
        r#"{"project":"proj","model":"m","thinking":"high"}"#,
    )
    .await;
    assert_eq!(answer.status, 202, "{}", answer.body);
    let asked = peer.asked();
    // The create's replay rule is the peer's, and it only holds if the key travels: a hub that minted its own would
    // create a second session for one retry.
    assert!(asked.contains("Idempotency-Key: retry-me\r\n"), "{asked}");
    assert!(asked.contains("Last-Event-ID: 41\r\n"), "{asked}");
    assert!(
        asked.contains("Content-Type: application/json\r\n"),
        "{asked}"
    );
    assert!(
        asked.starts_with("POST /api/sessions HTTP/1.1\r\n"),
        "{asked}"
    );
    // The body arrived whole, so the peer's `Content-Length` is this machine's to state.
    assert!(
        asked.contains(&format!("Content-Length: {}\r\n", 48)),
        "{asked}"
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_stream_is_relayed_by_its_event_bytes() {
    // A peer that answers `text/event-stream` in one chunk, with a `Last-Event-ID` request beside it: what must
    // arrive unchanged is the event bytes (`id:`/`data:`), never the transfer framing — the hub frames for its own
    // client (ADR-0074 D6).
    const EVENTS: &str =
        "id: 41\ndata: {\"kind\":\"turn.start\"}\n\nid: 42\ndata: {\"kind\":\"turn.end\"}\n\n";
    let dir = TempDir::new("forward-sse");
    let peer = Peer::start(move |_| {
        Some(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\n\
                 transfer-encoding: chunked\r\nconnection: close\r\n\r\n{:x}\r\n{EVENTS}\r\n0\r\n\r\n",
                EVENTS.len()
            )
            .into_bytes(),
        )
    })
    .await;
    let (served, _state) = hub(
        &dir,
        Config::default(),
        &[row(PEER, &peer.origin(), Some("http://127.0.0.1:1"))],
    )
    .await;
    let answer = request(
        served.addr,
        &format!(
            "GET /api/hosts/{PEER}/sessions/{ID}/events HTTP/1.1\r\nHost: 127.0.0.1\r\n\
             Last-Event-ID: 40\r\nConnection: close\r\n\r\n"
        ),
    )
    .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.header("content-type"), Some("text/event-stream"));
    // The peer framed its body in one chunk; the hub decoded it and `hyper` framed the answer again. What must be
    // equal is the **event bytes**, which is why the body is decoded first: a `contains` over the raw bytes would
    // pass just as happily over a peer's framing copied through verbatim.
    assert_eq!(
        String::from_utf8_lossy(&dechunk(&answer.bytes)),
        EVENTS,
        "the event bytes must travel unchanged"
    );
    assert!(
        peer.asked().contains("Last-Event-ID: 40\r\n"),
        "{}",
        peer.asked()
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_percent_encoded_tail_cannot_author_the_forwarded_head() {
    // `axum`'s `Path` percent-decodes, so a CRLF in the capture would be written into the request line this machine
    // sends a peer — a caller authoring the head the hub claims to author. The tail is taken raw.
    let dir = TempDir::new("forward-injected");
    let peer =
        Peer::answering("HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
    let (served, _state) = hub(
        &dir,
        Config::default(),
        &[row(PEER, &peer.origin(), Some("http://127.0.0.1:1"))],
    )
    .await;
    let answer = get(
        served.addr,
        &format!("/api/hosts/{PEER}/sessions%0d%0aX-Injected:%201"),
    )
    .await;
    let _ = answer;
    let asked = peer.asked();
    assert!(
        !asked
            .lines()
            .any(|line| line.to_ascii_lowercase().starts_with("x-injected")),
        "the injected header became a line of the forwarded head: {asked:?}"
    );
    assert!(
        asked.contains("%0d%0aX-Injected:%201"),
        "the tail lost the client's own encoding: {asked:?}"
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_reply_that_says_chunked_and_carries_a_length_keeps_the_chunks() {
    // A peer that sends both headers is not this build's hyper, and the decoded body's length is not the one that
    // header names: the framing wins, so the length is not copied and the answer is whole.
    const BODY: &str = "abc";
    let dir = TempDir::new("forward-both-framings");
    let peer = Peer::answering(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ntransfer-encoding: chunked\r\n\
         content-length: 3\r\nconnection: close\r\n\r\n3\r\nabc\r\n0\r\n\r\n",
    )
    .await;
    let (served, _state) = hub(
        &dir,
        Config::default(),
        &[row(PEER, &peer.origin(), Some("http://127.0.0.1:1"))],
    )
    .await;
    let answer = get(served.addr, &format!("/api/hosts/{PEER}/sessions")).await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(
        answer.header("content-length"),
        None,
        "the peer's length was copied onto a body it does not describe: {}",
        answer.body
    );
    assert_eq!(String::from_utf8_lossy(&dechunk(&answer.bytes)), BODY);
    served.finish().await.expect("a clean shutdown");
}

// ---- the decisions before any dial ------------------------------------------------------------------

#[tokio::test]
async fn local_is_this_machine_and_the_answer_is_a_redirect() {
    let dir = TempDir::new("forward-local");
    let (served, _state) = hub(&dir, Config::default(), &[]).await;
    let answer = get(served.addr, "/api/hosts/local/sessions?x=1").await;
    assert_eq!(answer.status, 308, "{}", answer.body);
    assert_eq!(answer.header("location"), Some("/api/sessions?x=1"));
    assert_eq!(answer.header("content-length"), Some("0"));
    assert!(answer.body.is_empty(), "{}", answer.body);
    // And no envelope: a redirect is not a refusal.
    assert!(
        !answer.body.contains("\"code\""),
        "the redirect carried an envelope: {}",
        answer.body
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_name_the_registry_does_not_hold_is_unknown_host() {
    let dir = TempDir::new("forward-unknown");
    let (served, _state) = hub(&dir, Config::default(), &[]).await;
    let answer = get(served.addr, "/api/hosts/nobody/sessions").await;
    assert_refusal(&answer, 404, UNKNOWN_HOST);
    assert!(answer.body.contains("nobody"), "{}", answer.body);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_registry_is_not_relayed() {
    // One hop is the whole promise: a hub that relayed `/api/hosts/…` would be a second-hand claim about a third
    // machine, and the registry is each machine's own (`GET`/`POST /api/hosts` are *this* machine's table).
    let dir = TempDir::new("forward-registry");
    let (served, _state) = hub(&dir, Config::default(), &[]).await;
    for (method, path) in [
        ("GET", format!("/api/hosts/{PEER}/hosts")),
        ("POST", format!("/api/hosts/{PEER}/hosts/{PEER}")),
    ] {
        let answer = send(served.addr, method, &path, &[], "").await;
        assert_refusal(&answer, 403, NOT_FORWARDED);
    }
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_server_has_no_forwarding_route_at_all() {
    // `Roles`: a hub forwards, a server serves the session API. The route is not registered, so the answer is the
    // fallback's envelope and not a refusal of this module's.
    let dir = TempDir::new("forward-server-role");
    let config = Config {
        role: Role::Server,
        ..Config::default()
    };
    let (served, _state) = hub(&dir, config, &[]).await;
    let answer = get(served.addr, &format!("/api/hosts/{PEER}/sessions")).await;
    assert_refusal(&answer, 404, NOT_FOUND);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_hub_creates_no_sessions_of_its_own() {
    // Spec's `Roles`: "a hub-only machine has no Docker and no sessions". The refusal is a machine fact, answered
    // before the body is even looked at, and no row is written.
    let dir = TempDir::new("forward-hub-create");
    let config = Config {
        role: Role::Hub,
        ..Config::default()
    };
    let (served, state) = hub(&dir, config, &[]).await;
    let answer = send(
        served.addr,
        "POST",
        "/api/sessions",
        &[("Idempotency-Key", "hub-1")],
        r#"{"project":"proj","model":"m","thinking":"high"}"#,
    )
    .await;
    assert_refusal(&answer, 403, SESSIONS_DISABLED);
    assert!(answer.body.contains("/api/hosts/"), "{}", answer.body);
    let rows = state
        .database
        .lock()
        .expect("the lock is not poisoned")
        .sessions()
        .list(None)
        .expect("the table reads");
    assert!(rows.is_empty(), "a hub wrote a session: {rows:?}");
    // The two reads a hub does serve.
    assert_eq!(get(served.addr, "/api/sessions").await.status, 200);
    assert_eq!(get(served.addr, "/api/hosts").await.status, 200);
    served.finish().await.expect("a clean shutdown");
}

// ---- the two failures -----------------------------------------------------------------------------

/// A port nothing listens on: bound and closed, so the kernel refuses the connection.
fn closed_port() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port is bindable");
    listener.local_addr().expect("a bound address")
}

#[tokio::test]
async fn a_peer_that_is_gone_is_a_bad_gateway_with_its_name_in_the_sentence() {
    let dir = TempDir::new("forward-gone");
    let gone = closed_port();
    let (served, _state) = hub(
        &dir,
        Config::default(),
        &[row(
            PEER,
            &format!("http://{gone}"),
            Some("http://127.0.0.1:1"),
        )],
    )
    .await;
    let answer = get(served.addr, &format!("/api/hosts/{PEER}/sessions")).await;
    assert_refusal(&answer, 502, UPSTREAM_UNREACHABLE);
    assert!(answer.body.contains(PEER), "{}", answer.body);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_peer_that_never_answers_is_a_gateway_timeout() {
    // Measured against a shortened bound: the constant is asserted as a value in `forward::tests` and in the gate,
    // and a test that waited five seconds would be a slow test of a fast rule.
    let dir = TempDir::new("forward-timeout-short");
    let peer = Peer::start(|_| None).await;
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    state
        .database
        .lock()
        .expect("the lock is not poisoned")
        .hosts()
        .upsert(&row(PEER, &peer.origin(), Some("http://127.0.0.1:1")))
        .expect("the row is stored");
    let mut state = state;
    state.forwarding = Forwarding {
        default: Duration::from_millis(120),
        slow: Duration::from_millis(120),
    };
    let served = Served::start(http::router(state), GRACE).await;
    let started = std::time::Instant::now();
    let answer = get(served.addr, &format!("/api/hosts/{PEER}/sessions")).await;
    assert_refusal(&answer, 504, UPSTREAM_TIMEOUT);
    assert!(answer.body.contains(PEER), "{}", answer.body);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the bound did not fire: {:?}",
        started.elapsed()
    );
    assert_eq!(peer.asked_count(), 1, "the peer was asked once");
    served.finish().await.expect("a clean shutdown");
}

// ---- the preview hop ------------------------------------------------------------------------------

/// A preview listener on a loopback port, serving this machine's previews as `role` says.
struct Preview {
    address: SocketAddr,
    stop: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl Preview {
    async fn start(dir: &TempDir, config: &Config, rows: &[Host]) -> Self {
        use holosmith::preview::Upstream;
        struct Never;
        impl Upstream for Never {
            fn address(&self, session: &str) -> Result<SocketAddr, preview::Refusal> {
                Err(preview::Refusal::NoContainer {
                    id: session.to_string(),
                    detail: "this test has no containers".to_string(),
                })
            }
        }
        let state = state_in(dir, config.clone());
        for row in rows {
            state
                .database
                .lock()
                .expect("the lock is not poisoned")
                .hosts()
                .upsert(row)
                .expect("the row is stored");
        }
        let context = preview::Context {
            origin: Origin::parse("http://localhost:8081").expect("a test origin"),
            role: config.role,
            database: Arc::clone(&state.database),
            upstream: Arc::new(Never),
            tools: Tools::default(),
            // this test's machine lists no hubs, so its preview listener verifies nobody (the config's own
            // default, and the arm the ACLs-only fallback is).
            trust: holosmith::trust::Trust::of(config, &Tools::default()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is bindable");
        let address = listener.local_addr().expect("a bound address");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(preview::serve(listener, Arc::new(context), async move {
            let _ = stopped.await;
        }));
        Self {
            address,
            stop,
            handle,
        }
    }

    async fn finish(self) {
        let _ = self.stop.send(());
        self.handle.await.expect("the listener stops cleanly");
    }
}

/// One request to a preview listener, read to the end.
async fn preview_request(address: SocketAddr, text: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("the listener accepts");
    stream
        .write_all(text.as_bytes())
        .await
        .expect("the request is written");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("the answer is read");
    String::from_utf8_lossy(&raw).to_string()
}

#[tokio::test]
async fn a_forwarded_preview_asks_the_peer_in_its_own_words() {
    let dir = TempDir::new("preview-forward");
    let peer = Peer::answering(
        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: 5\r\n\
         connection: close\r\n\r\nhello",
    )
    .await;
    let config = Config::default();
    let listener = Preview::start(
        &dir,
        &config,
        &[row(PEER, "http://127.0.0.1:1", Some(&peer.origin()))],
    )
    .await;
    let answer = preview_request(
        listener.address,
        &format!("GET /{PEER}/{ID}/js/app.js?x=1 HTTP/1.1\r\nHost: hub.example\r\n\r\n"),
    )
    .await;
    assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
    assert!(answer.ends_with("hello"), "{answer}");
    let asked = peer.asked();
    // The peer's own spelling of its own session, and its own preview origin in `Host` (and in `Origin`, which this
    // machine always authors — a `GET` is not refused for carrying one).
    assert!(
        asked.starts_with(&format!("GET /local/{ID}/js/app.js?x=1 HTTP/1.1\r\n")),
        "{asked}"
    );
    assert!(
        asked.contains(&format!("Host: {}\r\n", peer.address)),
        "{asked}"
    );
    listener.finish().await;
}

#[tokio::test]
async fn an_upgrade_reaches_the_peer_with_the_peers_own_origin() {
    // The hub checks the browser's `Origin` against its own preview origin, and the peer checks its own again —
    // which only works if this machine re-authors the header (ADR-0074 D5, and the reason `hosts.preview` exists).
    let dir = TempDir::new("preview-forward-upgrade");
    let peer = Peer::answering(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: c3R1Yg==\r\n\r\n",
    )
    .await;
    let config = Config::default();
    let listener = Preview::start(
        &dir,
        &config,
        &[row(PEER, "http://127.0.0.1:1", Some(&peer.origin()))],
    )
    .await;
    let answer = preview_request(
        listener.address,
        &format!(
            "GET /{PEER}/{ID}/ws HTTP/1.1\r\nHost: hub.example\r\nOrigin: http://localhost:8081\r\n\
             Upgrade: websocket\r\nConnection: Upgrade\r\n\r\n"
        ),
    )
    .await;
    assert!(answer.starts_with("HTTP/1.1 101"), "{answer}");
    let asked = peer.asked();
    assert!(
        asked.contains(&format!("Origin: {}\r\n", peer.origin())),
        "the peer must see its own origin: {asked}"
    );
    assert!(
        !asked.contains("Origin: http://localhost:8081"),
        "the hub's origin leaked to the peer: {asked}"
    );
    assert!(
        asked.contains("Connection: Upgrade\r\n"),
        "an upgrade keeps its connection header: {asked}"
    );
    listener.finish().await;
}

#[tokio::test]
async fn a_previews_four_refusals_say_which_fact_is_missing() {
    // A name the registry does not hold, a machine whose role forwards nothing, a row no probe has carried a
    // preview origin for, and a peer that is not there: four answers, four codes, all plain text (this listener has
    // no envelope).
    let dir = TempDir::new("preview-forward-refusals");
    let gone = closed_port();
    let config = Config::default();
    let listener = Preview::start(
        &dir,
        &config,
        &[
            row("silent", "http://127.0.0.1:1", None),
            row(
                "gone",
                "http://127.0.0.1:1",
                Some(&format!("http://{gone}")),
            ),
        ],
    )
    .await;
    for (path, status, needle) in [
        (
            format!("/nobody/{ID}/"),
            404,
            "not a host in this machine's registry",
        ),
        (
            format!("/silent/{ID}/"),
            502,
            "not where it serves previews",
        ),
        (format!("/gone/{ID}/"), 502, "could not be reached"),
    ] {
        let answer = preview_request(
            listener.address,
            &format!("GET {path} HTTP/1.1\r\nHost: hub.example\r\n\r\n"),
        )
        .await;
        assert!(
            answer.starts_with(&format!("HTTP/1.1 {status}")),
            "{path}: {answer}"
        );
        assert!(
            !answer.contains("\"code\""),
            "{path} answered an envelope: {answer}"
        );
        assert!(answer.contains(needle), "{path}: {answer}");
    }
    listener.finish().await;

    // And the role that forwards nothing: the same row, a machine that is a server.
    let dir = TempDir::new("preview-forward-refusals-role");
    let peer = Peer::answering("HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await;
    let config = Config {
        role: Role::Server,
        ..Config::default()
    };
    let listener = Preview::start(
        &dir,
        &config,
        &[row(PEER, "http://127.0.0.1:1", Some(&peer.origin()))],
    )
    .await;
    let answer = preview_request(
        listener.address,
        &format!("GET /{PEER}/{ID}/ HTTP/1.1\r\nHost: hub.example\r\n\r\n"),
    )
    .await;
    assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
    assert!(answer.contains("runs no forwarding"), "{answer}");
    assert_eq!(peer.asked_count(), 0, "a server dialled a peer");
    listener.finish().await;
}
