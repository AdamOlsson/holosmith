//! Forwarding one API call to another machine: the hub's client (spec item 3, ADR-0074).
//!
//! The hub's preview listener is a byte tunnel (ADR-0051) and stays one; this module is the *other* shape, because
//! the session API's answers have to travel through `axum` — the envelope, the four browser guards and the one
//! `.layer(` are the control listener's, and a listener cannot hand a socket to a router. So the hub **is a client
//! of one named peer here**: it writes one HTTP/1.1 request, reads the peer's reply head, copies the status and the
//! headers, and relays the body through [`BodyStream`] until the peer's own framing says it is over.
//!
//! What that costs, said plainly: the tunnel's whole selling point is that it cannot be wrong about a body it never
//! parses, and this file parses. Two facts bound the cost. **One direction only** — the peer's chunked framing is
//! decoded and `hyper` frames the answer downstream, so there is no re-encode to get wrong, and `Content-Length`
//! bodies need no decoder at all. And **the peer is this same binary**, whose answers are hyper's: one status line,
//! headers, and either a length, `Transfer-Encoding: chunked`, or a close.
//!
//! Three measured facts shaped this:
//!
//! - **`HTTP/1.0` is not the shortcut.** Asking a peer for a 1.0 answer would make its server close-delimit every
//!   body, which is exactly what a relay wants — but hyper's server will not close-delimit an endless one, so it
//!   sends no head at all: `curl -0` against `GET /api/sessions/<id>/events` on this build printed **zero bytes in
//!   four seconds** while the same request over 1.1 answered headers at once. Hence a decoder.
//! - **No new dependency.** Hyper's `client` feature would do the decoding for us and costs `want` (ADR-0051
//!   measured that and refused it for the preview listener). This build has hand-written a SHA-256, an AEAD, a
//!   ustar writer and a DER writer to avoid one crate (ADR-0071), and `keyproxy::read_chunked` already decodes
//!   chunked by hand — bounded, because a key proxy's body must be; this is its streaming sibling.
//! - **The bound is on the answer's head, never on its body.** Item 3's streams do not end, so a bound that
//!   covered the body would truncate an event stream. [`TIMEOUT`] and [`SLOW_TIMEOUT`] cover the connect, the
//!   request and the reply head; after the head there is no deadline at all.
//!
//! [`Link`] is the transport and the one seam a test can stand in: a plain socket for `http` and one `openssl
//! s_client` child for `https` — [`crate::hosts::probe`]'s child with a request that does not end, so its streams
//! stay open and `kill_on_drop` is what ends it. It is generic over `AsyncRead + AsyncWrite` at the far end, which
//! is also what lets the hub's **preview** hop use it (a TLS peer's preview listener is behind `s_client` too).

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Bytes;
use axum::http::HeaderMap;
use futures_core::Stream;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdin, ChildStdout};

use crate::config::Origin;
use crate::preflight::Tools;

/// How long a forwarded call may take to answer its **head**, when nothing says otherwise.
///
/// Spec item 3's rule 6 ("default forwarding timeout is 5 s") handed from ADR-0065 D8 and ADR-0066 D12. It covers
/// the dial, the request and the reply's head — never the body: an event stream has no natural end (ADR-0018).
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// How long the two routes that legitimately take their time get (spec item 3's "merge and publish use 60 s"),
/// plus the delete, which waits out the peer's own `abort_grace` (ten seconds by default) before it takes the
/// worktree's snapshot. The number is this machine's constant, not the peer's config, so a peer configured with a
/// longer grace can still outlast its arm and get a `504` — recorded in ADR-0074.
pub const SLOW_TIMEOUT: Duration = Duration::from_secs(60);

/// How long dialling a peer may take. Short, like the preview listener's: a machine on the tailnet either answers
/// or is not there.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest reply head this client reads, and the longest request head it writes.
pub const MAX_HEAD: usize = 16 * 1024;

/// The longest a chunk-size line may be before it is not one.
const MAX_CHUNK_LINE: usize = 1024;

/// How much is read from the peer at a time.
const READ: usize = 8 * 1024;

/// The two timeouts, as one value the state hands a handler.
///
/// A struct rather than two constants read at the call site because a test that measured `504` by waiting five
/// seconds would be a slow test of a fast rule — the seam [`crate::compactor::Compactor::with_window`] and
/// [`crate::push::Pusher::with_threshold`] have, for the same reason.
#[derive(Debug, Clone, Copy)]
pub struct Forwarding {
    pub default: Duration,
    pub slow: Duration,
}

impl Default for Forwarding {
    fn default() -> Self {
        Self {
            default: TIMEOUT,
            slow: SLOW_TIMEOUT,
        }
    }
}

impl Forwarding {
    /// What one forwarded call gets, by the method and the path this machine is relaying.
    ///
    /// Three arms, and each is a route this build has: `merge` and `publish` (the spec's own pair), and a `DELETE`
    /// of one session, which is slow because the **peer** is slow — its delete asks the agent to stop and waits out
    /// `abort_grace` before it snapshots the worktree ([`crate::http::sessions::deletion`]). A blanket five seconds
    /// would answer `504` for a delete the peer is executing, which is the failure an earlier development note predicted when
    /// ADR-0065 D8 left rule 6 to this story.
    pub fn timeout(&self, method: &str, rest: &str) -> Duration {
        let last = rest.rsplit('/').next().unwrap_or("");
        let slow = matches!(last, "merge" | "publish")
            || (method.eq_ignore_ascii_case("DELETE") && is_one_session(rest));
        if slow { self.slow } else { self.default }
    }
}

/// Whether `rest` is exactly one session's path (`sessions/<id>`), which is the delete's slow arm.
fn is_one_session(rest: &str) -> bool {
    let mut segments = rest.split('/');
    match (segments.next(), segments.next(), segments.next()) {
        (Some("sessions"), Some(id), None) => !id.is_empty(),
        _ => false,
    }
}

/// Why a forwarded call did not produce an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Nothing answered inside the bound: the peer is wedged, or the request never arrived.
    Timeout,
    /// The transport itself failed, or the peer answered something that is not HTTP.
    Sentence(String),
}

impl Failure {
    /// The sentence a `502` or a `504` carries: what was measured, and against which machine.
    pub fn sentence(&self, name: &str, origin: &Origin) -> String {
        match self {
            Self::Timeout => format!(
                "{name} did not answer at {} within the forwarding timeout, so this request was not completed",
                origin.as_str()
            ),
            Self::Sentence(detail) => {
                format!(
                    "{name} could not be reached at {}: {detail}",
                    origin.as_str()
                )
            }
        }
    }
}

/// The raw tail of `/api/hosts/<name>/<rest>`: the bytes the client wrote, **not** a decoded capture.
///
/// `axum`'s `Path` percent-decodes every parameter before a handler sees it, which is right for a name and wrong for
/// a path this machine writes into another machine's request line: a `%0d%0a` in `rest` would let a caller author a
/// header on the forwarded request — the exact thing [`request_head`] exists to prevent (the review found
/// it). Positional, so an encoded host segment cannot shift it, and `None` for anything that is not that shape.
pub fn raw_rest(path: &str) -> Option<&str> {
    let mut rest = path;
    // Three separators before the tail: `/api`, `/hosts`, `/<name>`, then what the peer is asked for.
    for _ in 0..3 {
        let tail = rest.strip_prefix('/')?;
        let at = tail.find('/')?;
        rest = &tail[at..];
    }
    rest.strip_prefix('/').filter(|rest| !rest.is_empty())
}

/// The authority of an origin: everything after `scheme://`.
pub fn authority(origin: &Origin) -> Option<&str> {
    origin.as_str().split_once("://").map(|(_, rest)| rest)
}

/// One request to write to a peer: the four things the head is made of.
pub struct Request<'a> {
    /// The client's method, unmapped: this machine relays what it was asked.
    pub method: &'a str,
    /// The forwarded path **below** `/api/`: `sessions/<id>/prompt`.
    pub rest: &'a str,
    /// The client's query, if it sent one.
    pub query: Option<&'a str>,
    /// The peer's origin, which becomes `Host` and `Origin`.
    pub origin: &'a Origin,
    /// The client's headers: three of them are the caller's business and are copied.
    pub headers: &'a HeaderMap,
    /// How many bytes of body follow. Recomputed from the buffered body, so the peer's framing is this machine's to
    /// state: the client may have sent `Transfer-Encoding: chunked` and this hop never relays an upload.
    pub length: usize,
}

/// The request head this machine sends a peer.
///
/// **The hub authors the call.** `Host` and `Origin` are the **peer's** own origin — its guard compares `Origin`
/// as text against its `control_origin`, and it answers `Host` against its `allowed_hosts` — and
/// `X-Holosmith-Client: 1` is this machine saying it is one. Three headers are the caller's and are copied
/// verbatim: `Content-Type` (what the body is), `Idempotency-Key` (which retry this is: a create replayed through
/// the hub must answer the peer's *row*, not a second session) and `Last-Event-ID` (where a stream resumes).
/// Everything else is dropped: hop-by-hop headers are the hop's, `Expect` would make the peer answer `100
/// Continue` to a request whose body is already in hand, and `transfer-encoding`/`content-length` are recomputed
/// here because the body is buffered.
///
/// **Consequence, stated rather than discovered:** a peer cannot tell a forwarded call from a call at its own
/// origin. That is exactly the hole /V11 closes (`allowed_hubs` and the whois), and the shape it has to
/// verify is this function's.
pub fn request_head(request: &Request<'_>) -> Option<String> {
    let authority = authority(request.origin)?;
    let mut head = String::with_capacity(256);
    head.push_str(request.method);
    head.push_str(" /api/");
    head.push_str(request.rest);
    if let Some(query) = request.query.filter(|query| !query.is_empty()) {
        head.push('?');
        head.push_str(query);
    }
    head.push_str(" HTTP/1.1\r\n");
    head.push_str(&format!("Host: {authority}\r\n"));
    head.push_str(&format!("Origin: {}\r\n", request.origin.as_str()));
    head.push_str("X-Holosmith-Client: 1\r\n");
    for (name, copied) in [
        ("content-type", "Content-Type"),
        ("idempotency-key", "Idempotency-Key"),
        ("last-event-id", "Last-Event-ID"),
    ] {
        if let Some(value) = request
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
        {
            head.push_str(&format!("{copied}: {value}\r\n"));
        }
    }
    // Every method that may carry a body states its length, `0` included: a request with neither
    // `Content-Length` nor `Transfer-Encoding` has no body by RFC 9110, and a hop that leaves the question open is
    // one a second proxy has to guess at. `GET`/`HEAD` say nothing, because a length on a `GET` reads as a body.
    if request.length > 0 || !matches!(request.method, "GET" | "HEAD") {
        head.push_str(&format!("Content-Length: {}\r\n", request.length));
    }
    head.push_str("Connection: close\r\n");
    head.push_str("\r\n");
    Some(head)
}

/// The reply's body as the peer framed it, which is what [`BodyStream`] has to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// Exactly this many octets, ending at the last one.
    Length(u64),
    /// `Transfer-Encoding: chunked`.
    Chunked,
    /// No length and no chunking: the body ends when the peer closes.
    UntilClose,
    /// No body at all: a status that carries none (`204`, `304`) or a `HEAD` this machine asked for.
    Done,
}

/// One reply's head, as the hub needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    /// Every header the peer sent, in its own order; [`keep_header`] decides which ones travel on.
    pub headers: Vec<(String, String)>,
    pub framing: Framing,
}

/// Whether a reply header may be copied onto this machine's answer.
///
/// The hop-by-hop set is the hop's: `hyper` frames the answer downstream, so a `Transfer-Encoding` copied from the
/// peer would be a second framing and `Connection`/`Keep-Alive`/`Upgrade`/`Trailer`/`TE`/`Proxy-*` describe a
/// connection that is not this one. **`Content-Length` is kept**, deliberately: the decoder's [`Framing::Length`]
/// makes "exactly N" a structural fact, it is what a bounded answer mostly is, and keeping it is what lets the gate
/// prove a forwarded row is the server's row byte for byte. A peer that dies with k of N sent makes the stream
/// error, and `hyper` aborts the answer after the head — a truncation the client can see, not a clean end.
pub fn keep_header(name: &str) -> bool {
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "transfer-encoding"
            | "trailer"
            | "te"
            | "upgrade"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
    )
}

/// Parse a reply head: the status line, the headers, and the body's framing.
///
/// `None` for anything that is not an HTTP status line, which is the one thing that makes a reply unreadable — a
/// peer that answers something else is not this build, and the caller says so.
pub fn parse_reply(head: &str) -> Option<Reply> {
    let mut lines = head.lines();
    let status_line = lines.next()?;
    let mut parts = status_line.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    let status = parts.next()?.parse::<u16>().ok()?;
    let mut headers = Vec::new();
    for line in lines {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':')?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    Some(Reply {
        framing: framing_of(status, &headers),
        status,
        headers,
    })
}

/// Which framing a status and its headers describe.
///
/// `Content-Length` wins over "no length" and loses to chunked — the order RFC 9112 §6.3 gives, and the order the
/// peer's own hyper writes them in.
fn framing_of(status: u16, headers: &[(String, String)]) -> Framing {
    if status == 204 || status == 304 || (100..200).contains(&status) {
        return Framing::Done;
    }
    let mut length = None;
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
            return Framing::Chunked;
        }
        if name == "content-length" {
            length = value.trim().parse::<u64>().ok().or(length);
        }
    }
    match length {
        Some(length) => Framing::Length(length),
        None => Framing::UntilClose,
    }
}

/// The transport to one peer: a plain socket, or one `openssl s_client` child's two pipes.
///
/// The child is ADR-0071/0073's: `-quiet` (the session chatter would otherwise be the answer's first line),
/// `-verify_return_error -verify_hostname` (a certificate that does not match is a refusal) and **no `-CAfile`**,
/// so the trust store is the process's own `SSL_CERT_FILE` — which is how a machine with a private CA configures
/// every tool it has, and how the gate can stand a TLS peer in on loopback.
pub enum Link {
    Plain(TcpStream),
    /// Boxed because the child's three handles are an order of magnitude larger than a socket: one `Link` per
    /// request, and a `Result<Link, Failure>` on the stack on every one of them.
    Tls(Box<Tls>),
}

/// One `openssl s_client` child: its two pipes, the child itself, and what it said.
pub struct Tls {
    /// Dropped with the link, and `kill_on_drop` is what ends the child: a forwarded stream outlives its request, so
    /// nothing else would. Underscored because it is **held**, never read — the name is the whole mechanism.
    _child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    /// The last non-empty line the child printed to stderr, read by a task that owns the pipe. Without it a refused
    /// certificate and an empty answer would read the same.
    said: Arc<Mutex<String>>,
}

impl Link {
    /// Dial `origin` and hand back the transport, or the sentence that says why not.
    pub async fn connect(origin: &Origin, tools: &Tools) -> Result<Self, Failure> {
        let Some(authority) = authority(origin) else {
            return Err(Failure::Sentence(format!(
                "{:?} is not an origin",
                origin.as_str()
            )));
        };
        let scheme = origin
            .as_str()
            .split_once("://")
            .map(|(scheme, _)| scheme)
            .unwrap_or_default();
        match scheme {
            "http" => {
                let target = origin.host().zip(origin.port());
                let Some((host, port)) = target else {
                    return Err(Failure::Sentence(format!(
                        "{authority} does not name a host and a port"
                    )));
                };
                let bare = host
                    .strip_prefix('[')
                    .and_then(|rest| rest.strip_suffix(']'))
                    .unwrap_or(host);
                let dial = TcpStream::connect((bare, port));
                match tokio::time::timeout(CONNECT_TIMEOUT, dial).await {
                    Ok(Ok(stream)) => Ok(Self::Plain(stream)),
                    Ok(Err(error)) => Err(Failure::Sentence(format!(
                        "the connection to {authority} failed ({error})"
                    ))),
                    Err(_) => Err(Failure::Sentence(format!(
                        "{authority} did not accept a connection within {} seconds",
                        CONNECT_TIMEOUT.as_secs()
                    ))),
                }
            }
            "https" => {
                let (Some(host), Some(_)) = (origin.host(), origin.port()) else {
                    return Err(Failure::Sentence(format!(
                        "{authority} does not name a host and a port"
                    )));
                };
                let bare = host
                    .strip_prefix('[')
                    .and_then(|rest| rest.strip_suffix(']'))
                    .unwrap_or(host);
                let mut command = tokio::process::Command::new(&tools.openssl);
                command
                    .args([
                        "s_client",
                        "-quiet",
                        "-connect",
                        authority,
                        "-servername",
                        bare,
                        "-verify_return_error",
                        "-verify_hostname",
                        bare,
                    ])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    // The one line that ends a child whose stream the client gave up on: a forwarded event stream
                    // outlives the request, so nothing else would stop it.
                    .kill_on_drop(true);
                let mut child = command.spawn().map_err(|error| {
                    Failure::Sentence(format!(
                        "the TLS client for {authority} could not be started ({error})"
                    ))
                })?;
                let stdin = child.stdin.take().ok_or_else(|| {
                    Failure::Sentence(format!("the TLS client for {authority} has no stdin"))
                })?;
                let stdout = child.stdout.take().ok_or_else(|| {
                    Failure::Sentence(format!("the TLS client for {authority} has no stdout"))
                })?;
                let said = Arc::new(Mutex::new(String::new()));
                if let Some(mut stderr) = child.stderr.take() {
                    let said = Arc::clone(&said);
                    tokio::spawn(async move {
                        let mut buffer = String::new();
                        let _ = stderr.read_to_string(&mut buffer).await;
                        if let Some(last) =
                            buffer.lines().rev().find(|line| !line.trim().is_empty())
                        {
                            *said
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                last.trim().to_string();
                        }
                    });
                }
                Ok(Self::Tls(Box::new(Tls {
                    _child: child,
                    stdin,
                    stdout,
                    said,
                })))
            }
            other => Err(Failure::Sentence(format!(
                "{:?} is not an http or https origin, so there is nothing to dial",
                other
            ))),
        }
    }

    /// What the TLS child said on stderr, if it said anything: the sentence a refused certificate needs.
    pub fn said(&self) -> Option<String> {
        match self {
            Self::Plain(_) => None,
            Self::Tls(tls) => {
                let said = &tls.said;
                let said = said
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (!said.is_empty()).then(|| said.clone())
            }
        }
    }

    /// Write the request head and body, inside the bound.
    pub async fn send(
        &mut self,
        head: &str,
        body: &[u8],
        deadline: tokio::time::Instant,
    ) -> Result<(), Failure> {
        let written = async {
            self.write_all(head.as_bytes()).await?;
            if !body.is_empty() {
                self.write_all(body).await?;
            }
            self.flush().await
        };
        match tokio::time::timeout_at(deadline, written).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(Failure::Sentence(format!(
                "the request did not reach the peer ({error})"
            ))),
            Err(_) => Err(Failure::Timeout),
        }
    }

    /// Read the reply's head — skipping any `1xx` interim one — and answer it with whatever body bytes arrived
    /// behind it.
    ///
    /// `1xx` is skipped rather than treated as the answer because a peer that sends `100 Continue` has not answered
    /// yet; `Expect` is stripped from the request this machine writes, so nothing here should arrive, and a parser
    /// that trusted its absence would be wrong the first time a peer was configured to send one.
    pub async fn read_reply(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<(Reply, Vec<u8>), Failure> {
        let mut buffer: Vec<u8> = Vec::with_capacity(1024);
        loop {
            let head = loop {
                if let Some(end) = find(&buffer, b"\r\n\r\n") {
                    break buffer.drain(..end + 4).collect::<Vec<u8>>();
                }
                if buffer.len() >= MAX_HEAD {
                    return Err(Failure::Sentence(
                        "the reply's head is longer than this machine reads".to_string(),
                    ));
                }
                let read = self.read_in(deadline, &mut buffer).await?;
                if read == 0 {
                    return Err(Failure::Sentence(match self.said() {
                        Some(said) => format!("the peer said nothing ({said})"),
                        None => "the peer closed before it answered".to_string(),
                    }));
                }
            };
            let Ok(text) = String::from_utf8(head) else {
                return Err(Failure::Sentence(
                    "the reply's head is not text".to_string(),
                ));
            };
            let Some(reply) = parse_reply(&text) else {
                return Err(Failure::Sentence(format!(
                    "the peer answered something that is not an HTTP status line ({:?})",
                    text.lines().next().unwrap_or_default()
                )));
            };
            if (100..200).contains(&reply.status) {
                continue;
            }
            return Ok((reply, buffer));
        }
    }

    /// One read into `buffer`, inside the bound. Zero is the peer's end of the stream.
    async fn read_in(
        &mut self,
        deadline: tokio::time::Instant,
        buffer: &mut Vec<u8>,
    ) -> Result<usize, Failure> {
        let mut chunk = [0u8; READ];
        match tokio::time::timeout_at(deadline, self.read(&mut chunk)).await {
            Ok(Ok(read)) => {
                buffer.extend_from_slice(&chunk[..read]);
                Ok(read)
            }
            Ok(Err(error)) => {
                let said = self.said();
                Err(Failure::Sentence(match said {
                    Some(said) => format!("{error} ({said})"),
                    None => error.to_string(),
                }))
            }
            Err(_) => Err(Failure::Timeout),
        }
    }
}

impl AsyncRead for Link {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(tls) => Pin::new(&mut tls.stdout).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Link {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(tls) => Pin::new(&mut tls.stdin).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(tls) => Pin::new(&mut tls.stdin).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(tls) => Pin::new(&mut tls.stdin).poll_shutdown(cx),
        }
    }
}

/// One reply's body, decoded as it arrives: what `axum` carries to the client.
///
/// A `Stream` rather than a copy loop because that is what `Body::from_stream` takes, and dropping it is what
/// cancels the whole hop: the link goes with it, and a `Link::Tls`'s child is killed on drop. So a client that
/// hangs up mid-stream, and a listener that is draining, both leave nothing behind — which is the rule the story
/// is judged on.
pub struct BodyStream<R> {
    reader: R,
    state: Decode,
    buffer: Vec<u8>,
    done: bool,
}

impl<R> BodyStream<R> {
    /// A body whose bytes start with `early` (`read_reply`'s leftovers) and continue from `reader`.
    pub fn new(reader: R, framing: Framing, early: Vec<u8>) -> Self {
        Self {
            reader,
            state: Decode::of(framing),
            buffer: early,
            done: false,
        }
    }
}

impl<R: AsyncRead + Unpin> Stream for BodyStream<R> {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if this.done {
                return Poll::Ready(None);
            }
            match piece(&mut this.buffer, &mut this.state) {
                Piece::Data(bytes) => return Poll::Ready(Some(Ok(bytes))),
                Piece::Done => {
                    this.done = true;
                    return Poll::Ready(None);
                }
                Piece::Malformed(detail) => {
                    this.done = true;
                    return Poll::Ready(Some(Err(io::Error::other(detail))));
                }
                Piece::More => {}
            }
            let mut chunk = [0u8; READ];
            let mut read = ReadBuf::new(&mut chunk);
            match Pin::new(&mut this.reader).poll_read(cx, &mut read) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => {
                    this.done = true;
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(Ok(())) => {
                    let filled = read.filled();
                    if filled.is_empty() {
                        this.done = true;
                        return Poll::Ready(match this.state {
                            // The peer closed exactly where its own framing said the body ended.
                            Decode::Done | Decode::UntilClose => {
                                this.state = Decode::Done;
                                None
                            }
                            // Or it closed in the middle, which is the one thing a client must not read as "the
                            // stream ended" (ADR-0074 D7): the answer is an error, and `hyper` aborts the reply.
                            _ => Some(Err(io::Error::other(
                                "the peer ended the body before its framing did",
                            ))),
                        });
                    }
                    this.buffer.extend_from_slice(filled);
                }
            }
        }
    }
}

/// The decoder's own state, which [`Framing`] becomes and then walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decode {
    Length { remaining: u64 },
    Chunk,
    ChunkData { remaining: u64 },
    ChunkEnd,
    Trailers,
    UntilClose,
    Done,
}

impl Decode {
    fn of(framing: Framing) -> Self {
        match framing {
            Framing::Length(0) | Framing::Done => Self::Done,
            Framing::Length(remaining) => Self::Length { remaining },
            Framing::Chunked => Self::Chunk,
            Framing::UntilClose => Self::UntilClose,
        }
    }
}

/// What one step of the decoder produced.
enum Piece {
    Data(Bytes),
    Done,
    More,
    Malformed(&'static str),
}

/// Take whatever whole piece the buffer already holds, advancing the state by one step.
///
/// The whole state machine, and deliberately a *streaming* one: a chunk is handed on as soon as it is complete, so a
/// dev-sized upload and an event stream are the same code with different pacing. `keyproxy::read_chunked` is the
/// bounded sibling and stays separate — it answers one `Vec<u8>` because a key proxy's body has a cap and its head
/// and body are one value there.
fn piece(buffer: &mut Vec<u8>, state: &mut Decode) -> Piece {
    loop {
        match *state {
            Decode::Done => return Piece::Done,
            Decode::Length { remaining } => {
                if remaining == 0 {
                    *state = Decode::Done;
                    return Piece::Done;
                }
                if buffer.is_empty() {
                    return Piece::More;
                }
                let take = remaining.min(buffer.len() as u64) as usize;
                *state = Decode::Length {
                    remaining: remaining - take as u64,
                };
                return Piece::Data(Bytes::from(buffer.drain(..take).collect::<Vec<u8>>()));
            }
            Decode::Chunk => {
                let Some(end) = find(buffer, b"\r\n") else {
                    if buffer.len() > MAX_CHUNK_LINE {
                        return Piece::Malformed("a chunk's size line did not end");
                    }
                    return Piece::More;
                };
                let size = std::str::from_utf8(&buffer[..end])
                    .ok()
                    .map(|line| line.split(';').next().unwrap_or("").trim())
                    .and_then(|hex| u64::from_str_radix(hex, 16).ok());
                let Some(size) = size else {
                    return Piece::Malformed("a chunk's size line was not hexadecimal");
                };
                buffer.drain(..end + 2);
                *state = if size == 0 {
                    Decode::Trailers
                } else {
                    Decode::ChunkData { remaining: size }
                };
            }
            Decode::ChunkData { remaining } => {
                if remaining == 0 {
                    *state = Decode::ChunkEnd;
                    continue;
                }
                if buffer.is_empty() {
                    return Piece::More;
                }
                let take = remaining.min(buffer.len() as u64) as usize;
                *state = Decode::ChunkData {
                    remaining: remaining - take as u64,
                };
                return Piece::Data(Bytes::from(buffer.drain(..take).collect::<Vec<u8>>()));
            }
            Decode::ChunkEnd => {
                if buffer.len() < 2 {
                    return Piece::More;
                }
                if &buffer[..2] != b"\r\n" {
                    return Piece::Malformed("a chunk did not end with a CRLF");
                }
                buffer.drain(..2);
                *state = Decode::Chunk;
            }
            Decode::Trailers => {
                let Some(end) = find(buffer, b"\r\n") else {
                    if buffer.len() > MAX_CHUNK_LINE {
                        return Piece::Malformed("a trailer line did not end");
                    }
                    return Piece::More;
                };
                // An empty line is the end of the trailers, and an empty buffer is an unfinished one.
                if end == 0 {
                    buffer.drain(..2);
                    *state = Decode::Done;
                    return Piece::Done;
                }
                buffer.drain(..end + 2);
            }
            Decode::UntilClose => {
                if buffer.is_empty() {
                    return Piece::More;
                }
                return Piece::Data(Bytes::from(std::mem::take(buffer)));
            }
        }
    }
}

/// The first position of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(text: &str) -> Origin {
        Origin::parse(text).expect("a test origin")
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("a header name"),
                axum::http::HeaderValue::from_str(value).expect("a header value"),
            );
        }
        headers
    }

    #[test]
    fn the_forwarded_head_is_the_peers_and_three_of_the_callers() {
        let origin = origin("https://workshop.tail1234.ts.net");
        let headers = headers(&[
            ("content-type", "application/json"),
            ("idempotency-key", "abc-123"),
            ("last-event-id", "41"),
            ("host", "hub.tail1234.ts.net"),
            ("origin", "https://hub.tail1234.ts.net"),
            ("connection", "keep-alive"),
            ("expect", "100-continue"),
            ("cookie", "session=1"),
        ]);
        let head = request_head(&Request {
            method: "POST",
            rest: "sessions/0123456789abcdef/prompt",
            query: Some("x=1"),
            origin: &origin,
            headers: &headers,
            length: 0,
        })
        .expect("a head");
        assert_eq!(
            head,
            "POST /api/sessions/0123456789abcdef/prompt?x=1 HTTP/1.1\r\n\
             Host: workshop.tail1234.ts.net\r\n\
             Origin: https://workshop.tail1234.ts.net\r\n\
             X-Holosmith-Client: 1\r\n\
             Content-Type: application/json\r\n\
             Idempotency-Key: abc-123\r\n\
             Last-Event-ID: 41\r\n\
             Content-Length: 0\r\n\
             Connection: close\r\n\r\n"
        );
        // The caller's own `Origin`, `Host`, `Connection` and `Expect` are gone: the hub is the author of the call.
        for absent in [
            "hub.tail1234.ts.net",
            "keep-alive",
            "100-continue",
            "cookie",
        ] {
            assert!(!head.contains(absent), "{absent} survived: {head}");
        }
    }

    #[test]
    fn the_forwarded_tail_is_the_clients_own_encoding() {
        assert_eq!(
            raw_rest("/api/hosts/workshop/sessions/0123/prompt"),
            Some("sessions/0123/prompt")
        );
        assert_eq!(raw_rest("/api/hosts/workshop/sessions"), Some("sessions"));
        // The client's spelling, not a decoded one: a `%2F` stays a `%2F`, which is what the peer would have seen.
        assert_eq!(
            raw_rest("/api/hosts/w%6frkshop/sessions/a%2Fb"),
            Some("sessions/a%2Fb")
        );
        // A CRLF the router let through cannot reach a request line, because nothing here decodes.
        assert_eq!(
            raw_rest("/api/hosts/workshop/sessions%0d%0aX-Injected:%201"),
            Some("sessions%0d%0aX-Injected:%201")
        );
        for path in [
            "/api/hosts",
            "/api/hosts/",
            "/api/hosts/workshop",
            "/api/hosts/workshop/",
            "/api/health",
            "/",
        ] {
            assert_eq!(raw_rest(path), None, "{path}");
        }
    }

    #[test]
    fn a_mutating_call_with_no_body_says_zero_and_a_get_says_nothing() {
        // `POST /abort` carries no body: the peer must be told the length is zero rather than left to infer it.
        let origin = origin("http://127.0.0.1:8080");
        let head = request_head(&Request {
            method: "POST",
            rest: "sessions/0123456789abcdef/abort",
            query: None,
            origin: &origin,
            headers: &HeaderMap::new(),
            length: 0,
        })
        .expect("a head");
        assert!(head.contains("Content-Length: 0\r\n"), "{head}");
    }

    #[test]
    fn a_call_with_no_body_carries_no_length() {
        let origin = origin("http://127.0.0.1:8080");
        let head = request_head(&Request {
            method: "GET",
            rest: "sessions",
            query: None,
            origin: &origin,
            headers: &HeaderMap::new(),
            length: 0,
        })
        .expect("a head");
        assert_eq!(
            head,
            "GET /api/sessions HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nOrigin: http://127.0.0.1:8080\r\n\
             X-Holosmith-Client: 1\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn the_answer_is_its_head_and_whatever_arrived_behind_it() {
        let reply = parse_reply(
            "HTTP/1.1 202 Accepted\r\ncontent-type: application/json\r\ncontent-length: 3\r\n\
             transfer-encoding: chunked\r\nconnection: close\r\n\r\n",
        )
        .expect("a reply");
        assert_eq!(reply.status, 202);
        assert_eq!(
            reply.framing,
            Framing::Chunked,
            "chunked wins over a length"
        );
        assert_eq!(
            reply.headers,
            [
                ("content-type".to_string(), "application/json".to_string()),
                ("content-length".to_string(), "3".to_string()),
                ("transfer-encoding".to_string(), "chunked".to_string()),
                ("connection".to_string(), "close".to_string()),
            ]
        );
    }

    #[test]
    fn the_framing_is_the_one_the_status_and_headers_describe() {
        assert_eq!(
            parse_reply("HTTP/1.1 200 OK\r\ncontent-length: 12\r\n\r\n")
                .expect("a reply")
                .framing,
            Framing::Length(12)
        );
        assert_eq!(
            parse_reply("HTTP/1.1 200 OK\r\n\r\n")
                .expect("a reply")
                .framing,
            Framing::UntilClose
        );
        for status in [204, 304] {
            assert_eq!(
                parse_reply(&format!(
                    "HTTP/1.1 {status} No\r\ncontent-length: 12\r\n\r\n"
                ))
                .expect("a reply")
                .framing,
                Framing::Done,
                "{status}"
            );
        }
    }

    #[test]
    fn a_status_line_that_is_not_one_is_not_a_reply() {
        for head in [
            "",
            "hello\r\n\r\n",
            "HTTP/1.1\r\n\r\n",
            "HTTP/1.1 OK\r\n\r\n",
        ] {
            assert!(parse_reply(head).is_none(), "{head:?}");
        }
    }

    #[test]
    fn the_hop_by_hop_headers_do_not_travel_and_the_lengths_do() {
        for kept in [
            "content-type",
            "content-length",
            "cache-control",
            "etag",
            "date",
            "x-anything",
        ] {
            assert!(keep_header(kept), "{kept} must travel");
        }
        for dropped in [
            "Connection",
            "keep-alive",
            "Transfer-Encoding",
            "Trailer",
            "TE",
            "Upgrade",
            "Proxy-Authenticate",
            "proxy-authorization",
            "Proxy-Connection",
        ] {
            assert!(!keep_header(dropped), "{dropped} must not travel");
        }
    }

    #[test]
    fn the_two_slow_arms_are_the_specs_and_the_deletes() {
        let forwarding = Forwarding::default();
        assert_eq!(forwarding.default, Duration::from_secs(5));
        assert_eq!(forwarding.slow, Duration::from_secs(60));
        for (method, rest) in [
            ("GET", "sessions"),
            ("GET", "sessions/0123456789abcdef"),
            ("GET", "sessions/0123456789abcdef/changes"),
            ("POST", "sessions"),
            ("POST", "sessions/0123456789abcdef/prompt"),
            ("POST", "sessions/0123456789abcdef/resume"),
            ("GET", "health"),
        ] {
            assert_eq!(
                forwarding.timeout(method, rest),
                TIMEOUT,
                "{method} {rest} must get the default"
            );
        }
        for (method, rest) in [
            ("POST", "sessions/0123456789abcdef/merge"),
            ("POST", "sessions/0123456789abcdef/publish"),
            ("DELETE", "sessions/0123456789abcdef"),
            ("delete", "sessions/0123456789abcdef"),
        ] {
            assert_eq!(
                forwarding.timeout(method, rest),
                SLOW_TIMEOUT,
                "{method} {rest} must get the slow arm"
            );
        }
        // A delete of something that is not one session is not the slow arm.
        assert_eq!(forwarding.timeout("DELETE", "sessions"), TIMEOUT);
        assert_eq!(
            forwarding.timeout("POST", "sessions/x/merge/again"),
            TIMEOUT
        );
    }

    /// A decoder driven to its end over a whole body, so a test reads like the bytes it is about.
    fn decode(framing: Framing, text: &str) -> (String, Option<String>) {
        let mut state = Decode::of(framing);
        let mut buffer = text.as_bytes().to_vec();
        let mut body = Vec::new();
        let mut error = None;
        loop {
            match piece(&mut buffer, &mut state) {
                Piece::Data(bytes) => body.extend_from_slice(&bytes),
                Piece::Done => break,
                Piece::More => break,
                Piece::Malformed(detail) => {
                    error = Some(detail.to_string());
                    break;
                }
            }
        }
        (String::from_utf8_lossy(&body).to_string(), error)
    }

    #[test]
    fn a_length_is_exactly_the_bytes_it_promised() {
        let (body, error) = decode(Framing::Length(11), "hello worldand more");
        assert_eq!(body, "hello world");
        assert!(error.is_none());
        assert_eq!(decode(Framing::Length(0), "ignored").0, "");
    }

    #[test]
    fn chunks_are_joined_and_extensions_and_trailers_are_skipped() {
        let (body, error) = decode(
            Framing::Chunked,
            "5;ext=1\r\nhello\r\n6\r\n world\r\n0\r\nX-Trailer: 1\r\n\r\n",
        );
        assert_eq!(body, "hello world");
        assert!(error.is_none());
        // The last chunk with no trailers at all.
        assert_eq!(decode(Framing::Chunked, "3\r\nabc\r\n0\r\n\r\n").0, "abc");
    }

    #[test]
    fn a_chunk_split_across_reads_is_still_one_body() {
        let mut state = Decode::of(Framing::Chunked);
        let mut buffer = b"5\r\nhel".to_vec();
        assert!(matches!(piece(&mut buffer, &mut state), Piece::Data(_)));
        // The rest of the chunk has not arrived, so the decoder asks for more rather than inventing it.
        assert!(matches!(piece(&mut buffer, &mut state), Piece::More));
        buffer.extend_from_slice(b"lo\r\n0\r\n\r\n");
        let Piece::Data(bytes) = piece(&mut buffer, &mut state) else {
            panic!("the rest of the chunk");
        };
        assert_eq!(&bytes[..], b"lo");
        assert!(matches!(piece(&mut buffer, &mut state), Piece::Done));
    }

    #[test]
    fn a_body_that_is_not_the_chunked_one_is_malformed() {
        assert_eq!(
            decode(Framing::Chunked, "zz\r\nx\r\n").1.as_deref(),
            Some("a chunk's size line was not hexadecimal")
        );
        assert_eq!(
            decode(Framing::Chunked, "1\r\nxX\r\n0\r\n\r\n")
                .1
                .as_deref(),
            Some("a chunk did not end with a CRLF")
        );
    }

    #[test]
    fn a_body_with_no_length_is_everything_until_the_close() {
        assert_eq!(decode(Framing::UntilClose, "all of it").0, "all of it");
    }

    /// Every byte a [`BodyStream`] yields, and the first error it ended with (if it ended badly).
    async fn drain<S>(mut stream: S) -> (Vec<u8>, Option<String>)
    where
        S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
    {
        let mut body = Vec::new();
        let mut error = None;
        while let Some(item) =
            std::future::poll_fn(|cx| Stream::poll_next(Pin::new(&mut stream), cx)).await
        {
            match item {
                Ok(bytes) => body.extend_from_slice(&bytes),
                Err(item) => {
                    error = Some(item.to_string());
                    break;
                }
            }
        }
        (body, error)
    }

    #[test]
    fn a_truncated_body_is_an_error_and_a_finished_one_is_not() {
        // The two halves of ADR-0074 D7: a stream that ends where its framing said (clean) and one that ends in
        // the middle (an error, which is what makes `hyper` abort the answer instead of pretending it is whole).
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let clean = runtime.block_on(drain(BodyStream::new(
            &b"hello"[..],
            Framing::Length(5),
            Vec::new(),
        )));
        assert_eq!(clean.0, b"hello");
        assert!(clean.1.is_none(), "{clean:?}");
        let chunked = runtime.block_on(drain(BodyStream::new(
            &b"3\r\nabc\r\n0\r\n\r\n"[..],
            Framing::Chunked,
            Vec::new(),
        )));
        assert_eq!(chunked.0, b"abc");
        assert!(chunked.1.is_none(), "{chunked:?}");
        for (framing, text) in [
            (Framing::Length(5), "hel"),
            (Framing::Chunked, "3\r\nabc\r\n"),
        ] {
            let truncated =
                runtime.block_on(drain(BodyStream::new(text.as_bytes(), framing, Vec::new())));
            assert!(
                truncated
                    .1
                    .as_deref()
                    .is_some_and(|error| error.contains("before its framing")),
                "{framing:?} {text:?}: {truncated:?}"
            );
        }
    }
}
