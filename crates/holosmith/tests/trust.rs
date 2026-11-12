//! Machine-to-machine trust as two listeners meet it (spec item 4, V11).
//!
//! `trust`'s own unit tests drive the decision table with a shim; this file drives the two listeners *over a
//! socket*, which is where the interesting parts live: the control listener reads the connection's address out of
//! `ConnectInfo` (so a router that never went through [`holosmith::serve::run`] would verify nobody), the preview
//! listener reads `peer_addr`, and each spells the same decision in its own words.
//!
//! The client is **bound to `127.0.0.2`** for every "a peer, not the front door" arm. On this machine that is the
//! only way to be an address other than `127.0.0.1` without root, and it is exactly the distinction the rule makes:
//! `tailscale serve` connects from `127.0.0.1`, and the rest of `127/8` is a peer like any other.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use holosmith::config::{Config, Role};
use holosmith::http::error::{PEER_NOT_ALLOWED, PEER_UNVERIFIABLE};
use holosmith::http::{self, Guards};
use holosmith::preflight::Tools;
use holosmith::preview;

mod support;
use support::{Response, Served, TempDir, parse, request, state_in};

const GRACE: Duration = Duration::from_secs(10);

/// The address a peer is measured from: loopback, and not the one `tailscale serve` uses.
const PEER: &str = "127.0.0.2";

/// One `tailscale` a test owns: it records every argv, and answers `whois --json <ip>` from two files the test
/// rewrites between requests, so the same listener can be asked about a hub, a stranger and a daemon that is not
/// there without a second boot.
struct Whois {
    dir: TempDir,
}

impl Whois {
    fn new(label: &str) -> Self {
        let whois = Self {
            // Its own directory, and not the database's: `TempDir::new` empties the path it is given, so one label
            // for both would wipe the shim the moment a test booted a machine beside it.
            dir: TempDir::new(&format!("{label}-whois")),
        };
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> '{log}'\n\
             if [ -f '{answer}' ]; then cat '{answer}'; exit 0; fi\n\
             if [ -f '{failure}' ]; then cat '{failure}' >&2; fi\n\
             exit 1\n",
            log = whois.dir.file("argv").display(),
            answer = whois.dir.file("answer").display(),
            failure = whois.dir.file("failure").display(),
        );
        let program = whois.dir.write("tailscale", &script);
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
                .expect("the shim is executable");
        }
        whois
    }

    /// The tailnet's answer: this node, and no other.
    fn node(&self, name: &str) {
        self.dir.write(
            "answer",
            &format!(r#"{{"Node":{{"Name":"{name}.tail1a2b.ts.net"}}}}"#),
        );
    }

    /// No node at all, which is an answer a whois can give.
    fn answerless(&self) {
        self.dir.write("answer", "{}");
    }

    /// A whois that cannot read the tailnet, in the words this machine's real one prints.
    fn failing(&self) {
        self.dir
            .write("failure", "failed to connect to local tailscaled");
        let answer = self.dir.file("answer");
        let _ = std::fs::remove_file(answer);
    }

    fn tools(&self) -> Tools {
        Tools {
            tailscale: self.dir.file("tailscale"),
            ..Tools::default()
        }
    }

    /// Every call the listener made, one `whois --json <ip>` per line.
    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.file("argv")).unwrap_or_default()
    }
}

fn config(hubs: &[&str]) -> Config {
    Config {
        allowed_hubs: hubs.iter().map(|hub| hub.to_string()).collect(),
        ..Config::default()
    }
}

/// One request over a socket **bound to `source`**, so the listener sees that address as its peer.
async fn request_from(source: &str, addr: SocketAddr, text: &str) -> Response {
    let socket = tokio::net::TcpSocket::new_v4().expect("an IPv4 socket");
    socket
        // Port 0: the kernel picks the source port, and the *address* is the whole point of the helper.
        .bind(format!("{source}:0").parse().expect("a source address"))
        .expect("the source address is bindable");
    let mut stream = socket.connect(addr).await.expect("the listener accepts");
    stream
        .write_all(text.as_bytes())
        .await
        .expect("the request is written");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("the answer is read");
    parse(&raw)
}

/// `GET <path>` from a peer, with the host header a guarded machine answers.
async fn fetch_as(source: &str, addr: SocketAddr, path: &str) -> Response {
    request_from(
        source,
        addr,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
    .await
}

fn code(response: &Response) -> String {
    let body: serde_json::Value = serde_json::from_str(&response.body)
        .unwrap_or_else(|_| panic!("an envelope: {}", response.body));
    body["code"].as_str().unwrap_or_default().to_string()
}

/// A listener booted with `config` and the shim's `tailscale`.
async fn booted(label: &str, whois: &Whois, config: Config) -> Served {
    // One directory per test: `TempDir::new`'s label is what keeps two tests in one binary from sharing a database
    // file (ADR-0009's measured same-process locking trap).
    let dir = TempDir::new(&format!("{label}-db"));
    let guards = Guards::from_config_with(&config, &whois.tools());
    Served::start_with(http::router(state_in(&dir, config)), guards, GRACE).await
}

#[tokio::test]
async fn a_listed_node_is_served_and_another_node_is_refused() {
    let whois = Whois::new("trust-arm");
    let served = booted("trust-arm", &whois, config(&["hub"])).await;

    whois.node("hub");
    let response = fetch_as(PEER, served.addr, "/api/sessions").await;
    assert_eq!(response.status, 200, "{}", response.body);
    // The identity came from the **address**, not from anything the caller wrote: the shim was asked about the
    // socket's peer and about nothing else.
    assert_eq!(
        whois.calls().trim(),
        "whois --json 127.0.0.2",
        "{}",
        whois.calls()
    );

    whois.node("stranger");
    let response = fetch_as(PEER, served.addr, "/api/sessions").await;
    assert_eq!(response.status, 403, "{}", response.body);
    assert_eq!(code(&response), PEER_NOT_ALLOWED, "{}", response.body);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_refusal_names_the_peer_and_never_the_configuration() {
    let whois = Whois::new("trust-words");
    // Two entries that are not English words: the refusal's own sentence says "hub" as the domain noun, so a list
    // holding that word could not tell a leak from prose.
    let served = booted("trust-words", &whois, config(&["workshop", "attic"])).await;
    whois.node("stranger");
    let response = fetch_as(PEER, served.addr, "/api/sessions").await;
    assert_eq!(response.status, 403, "{}", response.body);
    // The peer and the node the tailnet named for it are the caller's own facts; the list is this machine's.
    assert!(response.body.contains("127.0.0.2"), "{}", response.body);
    assert!(response.body.contains("stranger"), "{}", response.body);
    for entry in ["workshop", "attic"] {
        assert!(
            !response.body.contains(entry),
            "the answer names `{entry}`, which is this machine's configuration: {}",
            response.body
        );
    }
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_forged_login_header_changes_nothing() {
    let whois = Whois::new("trust-forged");
    // The forged value is **the listed name**, which is what makes this arm able to fail: an implementation that
    // honoured the header would serve this call (`200`), and the reference of a name nobody listed would pass either
    // way. `tailscale serve` is what adds this header, and item 4 says the peer must not trust one a caller supplies.
    let served = booted("trust-forged", &whois, config(&["workshop"])).await;
    whois.node("stranger");
    let response = request_from(
        PEER,
        served.addr,
        "GET /api/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nTailscale-User-Login: workshop\r\n\
         X-Holosmith-Client: 1\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(response.status, 403, "{}", response.body);
    assert_eq!(code(&response), PEER_NOT_ALLOWED, "{}", response.body);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn an_identity_that_cannot_be_read_is_its_own_code() {
    let whois = Whois::new("trust-unverifiable");
    let served = booted("trust-unverifiable", &whois, config(&["hub"])).await;
    whois.failing();
    let response = fetch_as(PEER, served.addr, "/api/sessions").await;
    assert_eq!(response.status, 403, "{}", response.body);
    assert_eq!(code(&response), PEER_UNVERIFIABLE, "{}", response.body);
    // The daemon's English is a log line, not an answer: a body that repeated it would be a body a client cannot
    // act on, and the fix is diagnosed by `doctor`'s own Tailscale finding.
    assert!(!response.body.contains("tailscaled"), "{}", response.body);
    served.finish().await.expect("a clean shutdown");

    // And an answer with no node in it is the same fact: a whois that ran and named nobody verified nobody.
    let whois = Whois::new("trust-empty-answer");
    let served = booted("trust-unverifiable", &whois, config(&["hub"])).await;
    whois.answerless();
    let response = fetch_as(PEER, served.addr, "/api/sessions").await;
    assert_eq!(code(&response), PEER_UNVERIFIABLE, "{}", response.body);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_front_door_is_served_without_a_whois() {
    let whois = Whois::new("trust-front-door");
    let served = booted("trust-front-door", &whois, config(&["hub"])).await;
    // `127.0.0.1` is what `tailscale serve` connects from (deploy step 6): a browser's call arrives from here, and
    // asking the tailnet about an address that is not in it would refuse every browser on the machine.
    let response = fetch_as("127.0.0.1", served.addr, "/api/sessions").await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(
        whois.calls().is_empty(),
        "the front door costs no child: {}",
        whois.calls()
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn an_empty_list_verifies_nobody() {
    let whois = Whois::new("trust-empty-list");
    let served = booted("trust-empty-list", &whois, Config::default()).await;
    whois.failing();
    // The documented fallback (V11's "If it fails" cell): with no list, tailnet ACLs are the only control and a peer
    // is served whatever the daemon says.
    let response = fetch_as(PEER, served.addr, "/api/sessions").await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(whois.calls().is_empty(), "{}", whois.calls());
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_health_probe_stays_exempt_from_both_lists() {
    // The exemption is the one ADR-0010 wrote and this story does not touch: a machine's own probe is what fills a
    // hub's registry, and a hub whose address a server has not verified yet could otherwise never be registered.
    let whois = Whois::new("trust-health");
    let served = booted("trust-health", &whois, config(&["hub"])).await;
    let response = fetch_as(PEER, served.addr, "/api/health").await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(whois.calls().is_empty(), "{}", whois.calls());
    served.finish().await.expect("a clean shutdown");
}

// ---- the preview listener: the same decision, this listener's own words -------------------------------

/// The upstream the preview listener resolves sessions against: this test's machine has no container.
struct Never;

impl preview::Upstream for Never {
    fn address(&self, session: &str) -> Result<SocketAddr, preview::Refusal> {
        Err(preview::Refusal::NoContainer {
            id: session.to_string(),
            detail: "this test has no containers".to_string(),
        })
    }
}

/// A preview listener booted with `config` and the shim's `tailscale`, answering for one connection per test.
async fn listener(
    label: &str,
    whois: &Whois,
    config: Config,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let dir = TempDir::new(&format!("{label}-db"));
    let state = state_in(&dir, Config::default());
    let tools = whois.tools();
    let mut context = preview::Context::of(
        &config,
        Arc::clone(&state.database),
        Arc::new(Never) as Arc<dyn preview::Upstream>,
        &tools,
    );
    context.role = Role::Server;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port is bindable");
    let address = listener.local_addr().expect("a bound address");
    let handle = tokio::spawn(async move {
        preview::serve(listener, Arc::new(context), std::future::pending()).await;
    });
    (address, handle)
}

#[tokio::test]
async fn the_preview_listener_refuses_a_stranger_in_plain_text() {
    let whois = Whois::new("trust-preview-stranger");
    let (address, handle) = listener("trust-preview-stranger", &whois, config(&["hub"])).await;
    whois.node("stranger");
    let id = "0123456789abcdef";
    let response = request_from(
        PEER,
        address,
        &format!("GET /local/{id}/ HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert_eq!(response.status, 403, "{}", response.body);
    // A sentence, not an envelope: this listener has no API surface (ADR-0051).
    assert!(
        response
            .header("content-type")
            .is_some_and(|value| value.starts_with("text/plain")),
        "{:?}",
        response.headers
    );
    assert!(!response.body.contains("code"), "{}", response.body);
    assert!(response.body.contains("127.0.0.2"), "{}", response.body);
    handle.abort();
}

#[tokio::test]
async fn a_listed_peer_reaches_the_preview_listeners_own_answers() {
    let whois = Whois::new("trust-preview-hub");
    let (address, handle) = listener("trust-preview-hub", &whois, config(&["hub"])).await;
    whois.node("hub");
    let id = "0123456789abcdef";
    let response = request_from(
        PEER,
        address,
        &format!("GET /local/{id}/ HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
    .await;
    // Not a `403`: the machine check passed, and what refused the request is the listener's ordinary "no such
    // session", which is what makes the second hop's gate a measurement rather than a claim.
    assert_eq!(response.status, 404, "{}", response.body);
    assert!(
        response.body.contains("no session is called"),
        "{}",
        response.body
    );
    handle.abort();
}

#[tokio::test]
async fn the_preview_listeners_front_door_is_its_own() {
    let whois = Whois::new("trust-preview-front");
    let (address, handle) = listener("trust-preview-front", &whois, config(&["hub"])).await;
    whois.failing();
    let id = "0123456789abcdef";
    let response = request(
        address,
        &format!("GET /local/{id}/ HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert_eq!(response.status, 404, "{}", response.body);
    assert!(whois.calls().is_empty(), "{}", whois.calls());
    handle.abort();
}
