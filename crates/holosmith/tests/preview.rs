//! The preview listener as a browser and a dev server meet it (HS-041, spec item 3, V7).
//!
//! Every test starts the real [`preview::serve`] on an ephemeral port and talks HTTP/1.1 over a socket, so
//! each answer is one a browser gets. The dev server is a stub the test owns — a listener that records the
//! head it was sent and answers what the test told it to — which is what makes "the prefix was stripped",
//! "the body went through" and "the upgrade was tunnelled" observable facts rather than claims.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use holosmith::config::Origin;
use holosmith::preview::{self, Context, Refusal, Upstream};
use holosmith::sessions::NewSession;

mod support;
use support::{Response, TempDir, open, request};

/// `Config::default().preview_origin`, which is the origin an upgrade must carry.
const ORIGIN: &str = "http://localhost:8081";

/// How long a test waits for an answer it expects, before calling it missing.
const PATIENCE: Duration = Duration::from_secs(10);

// ---- the dev server the test controls -------------------------------------------------------

/// What the stub dev server does with a connection once it has read the head.
#[derive(Clone, Copy)]
enum Dev {
    /// Answer this head and this body, then close.
    Answer(&'static str),
    /// Answer `101`, then echo every byte back — a WebSocket's shape from the proxy's side.
    Upgrade,
}

/// One exchange the dev server saw: the head it was sent, and the bytes that followed it.
type Seen = Arc<Mutex<Vec<String>>>;

/// A stub dev server on a loopback port, and the record of what it was sent.
async fn dev(behaviour: Dev) -> (SocketAddr, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is bindable");
    let address = listener.local_addr().expect("a bound address");
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let recorded = Arc::clone(&recorded);
            tokio::spawn(async move {
                let Some(head) =
                    holosmith::tunnel::read_head(&mut stream, 64 * 1024, PATIENCE).await
                else {
                    return;
                };
                let mut exchange = head.clone();
                match behaviour {
                    Dev::Answer(answer) => {
                        // The body a `POST` carried follows the head; take what is there, briefly.
                        let mut tail = [0u8; 4096];
                        if let Ok(Ok(read)) =
                            tokio::time::timeout(Duration::from_millis(300), stream.read(&mut tail))
                                .await
                        {
                            exchange.push_str(&String::from_utf8_lossy(&tail[..read]));
                        }
                        recorded
                            .lock()
                            .expect("the record is not poisoned")
                            .push(exchange);
                        let _ = stream.write_all(answer.as_bytes()).await;
                        let _ = stream.flush().await;
                    }
                    Dev::Upgrade => {
                        recorded
                            .lock()
                            .expect("the record is not poisoned")
                            .push(exchange);
                        let _ = stream
                            .write_all(
                                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                                  Connection: Upgrade\r\n\r\n",
                            )
                            .await;
                        let _ = stream.flush().await;
                        let mut buffer = [0u8; 1024];
                        loop {
                            match stream.read(&mut buffer).await {
                                Ok(0) | Err(_) => break,
                                Ok(read) => {
                                    if stream.write_all(&buffer[..read]).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    (address, seen)
}

/// The `Upstream` seam, answered by the test: an address, or the refusal a real one would give.
struct Stub {
    answer: Result<SocketAddr, Refusal>,
}

impl Upstream for Stub {
    fn address(&self, _session: &str) -> Result<SocketAddr, Refusal> {
        self.answer.clone()
    }
}

// ---- the listener under test ----------------------------------------------------------------

/// One preview listener on an ephemeral port, servable and stoppable.
struct Listening {
    address: SocketAddr,
    stop: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl Listening {
    async fn start(dir: &TempDir, upstream: Result<SocketAddr, Refusal>) -> (Self, String) {
        let database = Arc::new(Mutex::new(open(dir)));
        let id = {
            let database = database.lock().expect("the lock is not poisoned");
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
                    1_700_000_000,
                )
                .expect("a session is created")
                .id
        };
        let context = Context {
            origin: Origin::parse(ORIGIN).expect("the test origin is valid"),
            role: holosmith::config::Role::Both,
            database,
            upstream: Arc::new(Stub { answer: upstream }),
            tools: holosmith::preflight::Tools::default(),
        };
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is bindable");
        let address = listener.local_addr().expect("a bound address");
        let (stop, stopped) = oneshot::channel();
        let handle = tokio::spawn(preview::serve(listener, Arc::new(context), async move {
            let _ = stopped.await;
        }));
        (
            Self {
                address,
                stop,
                handle,
            },
            id,
        )
    }

    async fn finish(self) {
        let _ = self.stop.send(());
        self.handle.await.expect("the listener stops cleanly");
    }
}

/// A request with no `Connection: close`: the proxy sets that itself, and a test that sent it could not tell
/// which side did.
async fn get(address: SocketAddr, path: &str, headers: &[(&str, &str)]) -> Response {
    let mut raw = format!("GET {path} HTTP/1.1\r\nHost: localhost:8081\r\n");
    for (name, value) in headers {
        raw.push_str(&format!("{name}: {value}\r\n"));
    }
    raw.push_str("\r\n");
    request(address, &raw).await
}

// ---- the route ------------------------------------------------------------------------------

#[tokio::test]
async fn a_path_reaches_the_dev_server_with_its_prefix_stripped() {
    let dir = TempDir::new("preview-path");
    let (upstream, seen) = dev(Dev::Answer(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello",
    ))
    .await;
    let (listening, id) = Listening::start(&dir, Ok(upstream)).await;
    let response = get(
        listening.address,
        &format!("/local/{id}/index.html?x=1"),
        &[],
    )
    .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "hello");
    let seen = seen.lock().expect("the record is not poisoned").clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].starts_with("GET /index.html?x=1 HTTP/1.1\r\n"),
        "the prefix was not stripped: {:?}",
        seen[0]
    );
    assert!(
        seen[0].contains(&format!("Host: {upstream}\r\n")),
        "the upstream must see its own address as the host: {:?}",
        seen[0]
    );
    assert!(seen[0].contains("Connection: close\r\n"), "{:?}", seen[0]);
    assert!(
        !seen[0].contains("localhost:8081"),
        "the browser's host must not reach the dev server: {:?}",
        seen[0]
    );
    listening.finish().await;
}

#[tokio::test]
async fn a_body_that_arrived_with_the_head_still_reaches_the_dev_server() {
    let dir = TempDir::new("preview-body");
    let (upstream, seen) = dev(Dev::Answer(
        "HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\nok",
    ))
    .await;
    let (listening, id) = Listening::start(&dir, Ok(upstream)).await;
    let response = request(
        listening.address,
        &format!(
            "POST /local/{id}/submit HTTP/1.1\r\nHost: localhost:8081\r\nContent-Length: 5\r\n\r\nhello"
        ),
    )
    .await;
    assert_eq!(response.status, 201);
    let seen = seen.lock().expect("the record is not poisoned").clone();
    assert!(
        seen[0].starts_with("POST /submit HTTP/1.1\r\n"),
        "{:?}",
        seen[0]
    );
    assert!(
        seen[0].ends_with("hello"),
        "the body was left behind: {:?}",
        seen[0]
    );
    listening.finish().await;
}

// ---- what this listener is not --------------------------------------------------------------

#[tokio::test]
async fn the_control_api_is_not_on_this_listener() {
    let dir = TempDir::new("preview-no-api");
    let (upstream, seen) = dev(Dev::Answer(
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nno",
    ))
    .await;
    let (listening, _) = Listening::start(&dir, Ok(upstream)).await;
    for path in ["/api/health", "/api/sessions", "/"] {
        let response = get(listening.address, path, &[]).await;
        assert_eq!(response.status, 404, "{path}");
        assert!(
            !response.body.contains("\"code\""),
            "{path} answered the control API's envelope: {}",
            response.body
        );
        assert!(
            response.body.contains('/'),
            "{path} answered no sentence: {}",
            response.body
        );
    }
    assert!(
        seen.lock().expect("the record is not poisoned").is_empty(),
        "a path this listener does not serve reached the dev server"
    );
    listening.finish().await;
}

#[tokio::test]
async fn another_machines_host_is_a_404_and_a_session_that_is_not_one_is_a_404() {
    let dir = TempDir::new("preview-404");
    let (upstream, seen) = dev(Dev::Answer(
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nno",
    ))
    .await;
    let (listening, _) = Listening::start(&dir, Ok(upstream)).await;
    for path in [
        "/studio/0123456789abcdef/",
        "/local/notahexid/",
        "/local/0123456789abcdef0/",
        // A well-formed id no row holds is the next test's; this one is the malformed shape.
        "/local/../",
    ] {
        let response = get(listening.address, path, &[]).await;
        assert_eq!(response.status, 404, "{path}");
    }
    assert!(
        seen.lock().expect("the record is not poisoned").is_empty(),
        "a refused path reached the dev server"
    );
    listening.finish().await;
}

#[tokio::test]
async fn a_session_the_table_does_not_hold_is_a_404() {
    let dir = TempDir::new("preview-unknown");
    let (upstream, _) = dev(Dev::Answer(
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nno",
    ))
    .await;
    let (listening, _) = Listening::start(&dir, Ok(upstream)).await;
    let response = get(listening.address, "/local/0123456789abcdef/", &[]).await;
    assert_eq!(response.status, 404);
    assert!(
        response.body.contains("0123456789abcdef"),
        "the sentence must name the id: {}",
        response.body
    );
    listening.finish().await;
}

#[tokio::test]
async fn a_session_with_no_container_is_a_502_that_names_the_reason() {
    let dir = TempDir::new("preview-no-container");
    let (listening, id) = Listening::start(
        &dir,
        Err(Refusal::NoContainer {
            id: "irrelevant".to_string(),
            detail: "it is not running".to_string(),
        }),
    )
    .await;
    let response = get(listening.address, &format!("/local/{id}/"), &[]).await;
    assert_eq!(response.status, 502);
    assert!(response.body.contains("not running"), "{}", response.body);
    listening.finish().await;
}

#[tokio::test]
async fn a_dev_server_that_answers_nothing_is_a_502() {
    let dir = TempDir::new("preview-unreachable");
    // A port the kernel gives out and nothing listens on: the connect is refused.
    let dead = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is bindable");
    let address = dead.local_addr().expect("a bound address");
    drop(dead);
    let (listening, id) = Listening::start(&dir, Ok(address)).await;
    let response = get(listening.address, &format!("/local/{id}/"), &[]).await;
    assert_eq!(response.status, 502);
    assert!(
        response.body.contains(&address.to_string()),
        "the sentence must name the address: {}",
        response.body
    );
    listening.finish().await;
}

// ---- item 3's one rule ----------------------------------------------------------------------

#[tokio::test]
async fn an_upgrade_from_the_preview_origin_is_tunnelled() {
    let dir = TempDir::new("preview-upgrade");
    let (upstream, seen) = dev(Dev::Upgrade).await;
    let (listening, id) = Listening::start(&dir, Ok(upstream)).await;
    let mut stream = TcpStream::connect(listening.address)
        .await
        .expect("the preview listener accepts");
    stream
        .write_all(
            format!(
                "GET /local/{id}/socket HTTP/1.1\r\nHost: localhost:8081\r\nOrigin: {ORIGIN}\r\n\
                 Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZQ==\r\n\
                 Sec-WebSocket-Version: 13\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("the handshake is written");
    let mut buffer = [0u8; 1024];
    let read = tokio::time::timeout(PATIENCE, stream.read(&mut buffer))
        .await
        .expect("the answer arrives")
        .expect("the answer is read");
    let answer = String::from_utf8_lossy(&buffer[..read]).to_string();
    assert!(
        answer.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
        "the upstream's own 101 must reach the browser: {answer}"
    );
    // One frame's worth of bytes each way: after a 101 the connection is opaque, which is what a tunnel means.
    stream
        .write_all(b"a-frame-of-bytes")
        .await
        .expect("the frame is written");
    let echoed = tokio::time::timeout(PATIENCE, stream.read(&mut buffer))
        .await
        .expect("the echo arrives")
        .expect("the echo is read");
    assert_eq!(&buffer[..echoed], b"a-frame-of-bytes");
    let seen = seen.lock().expect("the record is not poisoned").clone();
    assert!(
        seen[0].starts_with("GET /socket HTTP/1.1\r\n"),
        "{:?}",
        seen[0]
    );
    assert!(
        seen[0].contains("Connection: Upgrade\r\n"),
        "an upgrade must keep the client's Connection header: {:?}",
        seen[0]
    );
    listening.finish().await;
}

#[tokio::test]
async fn an_upgrade_from_another_origin_is_refused_and_never_reaches_the_dev_server() {
    let dir = TempDir::new("preview-foreign");
    let (upstream, seen) = dev(Dev::Upgrade).await;
    let (listening, id) = Listening::start(&dir, Ok(upstream)).await;
    let response = get(
        listening.address,
        &format!("/local/{id}/socket"),
        &[
            ("Origin", "http://evil.example"),
            ("Upgrade", "websocket"),
            ("Connection", "Upgrade"),
        ],
    )
    .await;
    assert_eq!(response.status, 403);
    assert!(
        response.body.contains(ORIGIN) && response.body.contains("evil.example"),
        "the sentence must name both origins: {}",
        response.body
    );
    assert!(
        seen.lock().expect("the record is not poisoned").is_empty(),
        "a refused upgrade reached the dev server"
    );
    listening.finish().await;
}

#[tokio::test]
async fn an_upgrade_with_no_origin_is_refused() {
    let dir = TempDir::new("preview-no-origin");
    let (upstream, seen) = dev(Dev::Upgrade).await;
    let (listening, id) = Listening::start(&dir, Ok(upstream)).await;
    let response = get(
        listening.address,
        &format!("/local/{id}/socket"),
        &[("Upgrade", "websocket"), ("Connection", "Upgrade")],
    )
    .await;
    assert_eq!(response.status, 403);
    assert!(
        response.body.contains("carried none"),
        "the sentence must say the header was missing: {}",
        response.body
    );
    assert!(seen.lock().expect("the record is not poisoned").is_empty());
    listening.finish().await;
}

#[tokio::test]
async fn a_slashless_path_is_redirected_to_the_slashed_one() {
    // A page served from `/local/<id>` has that as its base URL, so its relative `./assets/x.js` would resolve
    // to `/local/assets/x.js` — a different host segment, and a 404. The browser is sent to the slashed path,
    // which is also the one the session screen links to. An upgrade is not a page and is not redirected.
    let dir = TempDir::new("preview-slash");
    let (upstream, seen) = dev(Dev::Answer(
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello",
    ))
    .await;
    let (listening, id) = Listening::start(&dir, Ok(upstream)).await;
    let response = get(listening.address, &format!("/local/{id}"), &[]).await;
    assert_eq!(response.status, 308);
    assert_eq!(response.header("location"), Some(&*format!("/local/{id}/")));
    assert!(
        seen.lock().expect("the record is not poisoned").is_empty(),
        "a redirected path reached the dev server"
    );
    // The slashed path is served, and the query survives the redirect.
    let response = get(listening.address, &format!("/local/{id}/?x=1"), &[]).await;
    assert_eq!(response.status, 200);
    let seen = seen.lock().expect("the record is not poisoned").clone();
    assert!(
        seen[0].starts_with("GET /?x=1 HTTP/1.1\r\n"),
        "{:?}",
        seen[0]
    );
    listening.finish().await;
}

// ---- what item 3 does not ask for ------------------------------------------------------------

#[tokio::test]
async fn a_plain_get_with_another_origin_is_served() {
    // Item 3 checks the Origin on an **upgrade**. A preview has no auth by design — the spec accepts that
    // previews share an origin and are readable by anyone who can reach it — so a plain read is not refused,
    // and this test is the decision written down (ADR-0051).
    let dir = TempDir::new("preview-plain-origin");
    let (upstream, _) = dev(Dev::Answer(
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello",
    ))
    .await;
    let (listening, id) = Listening::start(&dir, Ok(upstream)).await;
    let response = get(
        listening.address,
        &format!("/local/{id}/"),
        &[("Origin", "http://evil.example")],
    )
    .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "hello");
    listening.finish().await;
}

#[tokio::test]
async fn every_method_is_proxied() {
    // A dev server's form post, its `PUT` and its `DELETE` are the dev server's business: this listener has no
    // method policy because item 3 names none, and a preview that refused a form would be a proxy that broke
    // the thing it exists to show.
    let dir = TempDir::new("preview-methods");
    let (upstream, seen) = dev(Dev::Answer(
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
    ))
    .await;
    let (listening, id) = Listening::start(&dir, Ok(upstream)).await;
    for method in ["PUT", "DELETE", "PATCH", "OPTIONS"] {
        let response = request(
            listening.address,
            &format!("{method} /local/{id}/thing HTTP/1.1\r\nHost: localhost:8081\r\n\r\n"),
        )
        .await;
        assert_eq!(response.status, 200, "{method}");
    }
    let seen = seen.lock().expect("the record is not poisoned").clone();
    assert_eq!(seen.len(), 4, "{seen:?}");
    listening.finish().await;
}
