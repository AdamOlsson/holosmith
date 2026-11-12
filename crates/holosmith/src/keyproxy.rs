//! `holosmith proxy key`: the plaintext-to-TLS side of the key proxy (spec item 2).
//!
//! Pi is configured with a provider `baseUrl` of `http://key-proxy:8080/<provider>` and a per-session
//! **dummy** key. A request therefore arrives here in plaintext, on a session network, carrying a value that
//! is not a credential. This module does four things, in this order, and nothing else:
//!
//! 1. **Select a provider by path.** The first segment is a row in [`crate::key::PROVIDERS`]; there is no
//!    request-controlled host or port, so a compromised agent cannot turn this into a tunnel to the control
//!    listener. This is ADR-0045's exact-list rule applied to the other proxy.
//! 2. **Validate the dummy key.** It must be a value in `keys.toml`'s `[sessions]` table. The session id it
//!    names is the log line's attribution, exactly as the egress proxy's username is.
//! 3. **Swap the real key in.** The dummy is removed and the provider's real key is put in the header that
//!    provider uses (`x-api-key` or `Authorization: Bearer`). The real key is read from the mounted
//!    `keys.toml` at request time, never from an environment variable or a command line.
//! 4. **Forward over TLS and stream back.** `openssl s_client` is the TLS client because the artifact is
//!    static and has no TLS stack; the agent image carries the binary (measured). Response bytes are copied
//!    as they arrive, so an SSE body is not buffered behind a `Content-Length` that never comes.
//!
//! **The network is the first half of "only session networks".** The container publishes no port, listens on
//! `0.0.0.0:8080`, and is joined only to the machine's `egress` network and each session's `agents_<id>`
//! network with the alias `key-proxy`. A dummy key is the second half: a request from the wrong network still
//! needs one. No address list is read from Docker here, and the ADR says why that is enough.
//!
//! The split is the same one `egress.rs` has: [`Route::parse`] and [`upstream`] decide from values and can be
//! tested without a socket; the tokio half moves bytes through that decision.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

use crate::key::{self, Auth, Provider, Store};

/// Where the proxy listens unless `--listen` says otherwise.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:8080";

/// The longest request head this proxy reads. A provider request is a request line, a handful of headers and
/// a JSON body; the bound keeps a client that never sends a blank line from holding a task.
pub const MAX_HEAD: usize = 64 * 1024;

/// The longest request body this proxy will hold. Provider prompts can be large, but a value beyond this is a
/// mistake or an attempt to make the proxy the memory the container is not allowed to be.
pub const MAX_BODY: usize = 32 * 1024 * 1024;

/// How many connections the machine-wide proxy will serve at once. Each may hold a `MAX_BODY`, so an
/// unbounded accept loop would let one session spend the memory every other session's model calls need.
pub const MAX_CONNECTIONS: usize = 64;

/// How long a client may take to send its head.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the TLS client may take to answer its first byte. A provider that never answers is a 502.
pub const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(120);

/// The CA bundle the agent image carries.
pub const CA_FILE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// The `openssl` program the proxy asks for TLS. See the module docs for why this is a process.
pub const OPENSSL: &str = "openssl";

/// One request selected by its path, before the key is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub provider: &'static Provider,
    /// The path the provider's API expects, with the key proxy's `/<provider>` prefix removed.
    pub path: String,
}

impl Route {
    /// Parse an origin-form request target (`/anthropic/v1/messages?beta=…`).
    ///
    /// The provider segment is matched exactly against [`key::PROVIDERS`]. A path that does not start with
    /// `/`, a provider this build does not know, or a `..` segment is refused with a sentence: there is no
    /// fallback host to send it to.
    pub fn parse(target: &str) -> Result<Self, RouteError> {
        let (path, query) = match target.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (target, None),
        };
        let Some(rest) = path.strip_prefix('/') else {
            return Err(RouteError::NotAPath(target.to_string()));
        };
        let mut segments = rest.splitn(2, '/');
        let id = segments.next().unwrap_or_default();
        if id.is_empty() {
            return Err(RouteError::NoProvider(target.to_string()));
        }
        let provider = key::provider(id).ok_or_else(|| RouteError::UnknownProvider {
            provider: id.to_string(),
        })?;
        let tail = segments.next().unwrap_or_default();
        let mut path = format!(
            "{}{}",
            provider.base_path,
            if tail.is_empty() { "" } else { "/" }
        );
        path.push_str(tail);
        if path.is_empty() {
            path.push('/');
        }
        if path.split('/').any(|segment| segment == "..") {
            return Err(RouteError::Traversal(target.to_string()));
        }
        let path = match query {
            Some(query) => format!("{path}?{query}"),
            None => path,
        };
        Ok(Self { provider, path })
    }
}

/// Why a request target is not a route this build can forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    NotAPath(String),
    NoProvider(String),
    UnknownProvider { provider: String },
    Traversal(String),
}

impl RouteError {
    pub fn status(&self) -> u16 {
        match self {
            Self::UnknownProvider { .. } => 403,
            Self::NotAPath(_) | Self::NoProvider(_) | Self::Traversal(_) => 400,
        }
    }

    pub fn word(&self) -> &'static str {
        match self {
            Self::NotAPath(_) => "not-a-path",
            Self::NoProvider(_) => "no-provider",
            Self::UnknownProvider { .. } => "unknown-provider",
            Self::Traversal(_) => "traversal",
        }
    }

    pub fn sentence(&self) -> String {
        match self {
            Self::NotAPath(target) => format!(
                "{target:?} is not an origin-form path: send /<provider>/… to the key proxy"
            ),
            Self::NoProvider(target) => {
                format!("{target:?} has no provider segment: send /<provider>/… to the key proxy")
            }
            Self::UnknownProvider { provider } => format!(
                "\"{provider}\" is not a provider this build forwards to ({}), so no key was used",
                key::provider_ids().join(", ")
            ),
            Self::Traversal(target) => {
                format!("{target:?} contains a .. segment, which is refused")
            }
        }
    }
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sentence())
    }
}

/// One parsed request head: method, route and the headers the forward needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub route: Route,
    /// Header names and values, with the original spelling. Names are compared case-insensitively.
    pub headers: Vec<(String, String)>,
    /// The body length the client declared, if it declared one.
    pub content_length: Option<usize>,
    /// Whether the client used chunked transfer encoding.
    pub chunked: bool,
}

impl Head {
    /// The first value of `name`, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The session's dummy key, from either header pi's providers use.
    pub fn dummy(&self) -> Option<&str> {
        if let Some(value) = self.header("x-api-key") {
            return Some(value.trim());
        }
        let authorization = self.header("authorization")?.trim();
        authorization
            .strip_prefix("Bearer ")
            .or_else(|| authorization.strip_prefix("bearer "))
            .map(str::trim)
    }
}

/// Parse a request head (`GET /x HTTP/1.1\r\n…\r\n\r\n`), leaving the body to the caller.
pub fn parse(head: &str) -> Result<Head, RouteError> {
    let mut lines = head
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line));
    let request = lines
        .next()
        .ok_or_else(|| RouteError::NotAPath(head.to_string()))?;
    let mut fields = request.split(' ').filter(|field| !field.is_empty());
    let method = fields.next().unwrap_or_default().to_string();
    let target = fields.next().unwrap_or_default();
    let version = fields.next().unwrap_or_default();
    if method.is_empty() || target.is_empty() || !version.starts_with("HTTP/") {
        return Err(RouteError::NotAPath(request.to_string()));
    }
    let route = Route::parse(target)?;
    let mut headers = Vec::new();
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_string();
        let value = value.trim().to_string();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse::<usize>().ok();
        }
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
        headers.push((name, value));
    }
    Ok(Head {
        method,
        route,
        headers,
        content_length,
        chunked,
    })
}

/// Build the request that goes over TLS: the provider's path, the real key in the header it uses, the body
/// the client sent, and only headers that mean the same thing upstream.
///
/// **The body is part of the request, not an argument to its length.** A head with a `Content-Length` and no
/// bytes is a request the provider waits out and then drops, which is the shape shipped and that story's
/// V3 run measured: every model call failed with `the provider closed the connection before answering`, and
/// the unit tests passed because they asserted the header.
///
/// **Hop-by-hop headers are dropped** (`Connection`, `Proxy-Connection`, `Keep-Alive`, `TE`, `Trailer`,
/// `Transfer-Encoding`, `Content-Length`, `Expect`) because each one describes the connection to *this* proxy.
/// `Host` is replaced by the provider's host, and the provider's auth header is replaced by the real key.
/// Everything else is passed through — an Anthropic `anthropic-beta` or an OpenAI `OpenAI-Beta` is part of the
/// request's meaning, not of the transport.
pub fn upstream(head: &Head, real_key: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!("{} {} HTTP/1.1\r\n", head.method, head.route.path);
    out.push_str(&format!("Host: {}\r\n", head.route.provider.host));
    out.push_str("Connection: close\r\n");
    for (name, value) in &head.headers {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "host"
                | "connection"
                | "proxy-connection"
                | "keep-alive"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "content-length"
                | "expect"
                | "authorization"
                | "x-api-key"
        ) {
            continue;
        }
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    match head.route.provider.auth {
        Auth::XApiKey => out.push_str(&format!("x-api-key: {real_key}\r\n")),
        Auth::Bearer => out.push_str(&format!("Authorization: Bearer {real_key}\r\n")),
    }
    if !body.is_empty() || head.content_length.is_some() || head.chunked {
        out.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    out.push_str("\r\n");
    let mut request = out.into_bytes();
    request.extend_from_slice(body);
    request
}

/// What one answered request logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub session: String,
    pub provider: &'static str,
    pub host: &'static str,
    pub status: &'static str,
    pub up: u64,
    pub down: u64,
}

impl Record {
    pub fn refused(
        session: &str,
        provider: &'static str,
        host: &'static str,
        status: &'static str,
    ) -> Self {
        Self {
            session: session.to_string(),
            provider,
            host,
            status,
            up: 0,
            down: 0,
        }
    }

    /// One line per request, at `info` because a request is a fact an operator asked to read (the egress
    /// proxy's line is the template).
    pub fn log(&self) {
        tracing::info!(
            target: "holosmith::keyproxy",
            session = %self.session,
            provider = %self.provider,
            host = %self.host,
            status = %self.status,
            bytes_up = self.up,
            bytes_down = self.down,
            "the key proxy answered a request"
        );
    }
}

/// Serve forever, one task per connection.
pub async fn serve(listener: TcpListener, store: Arc<Store>) {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let Ok(permit) = Arc::clone(&slots).acquire_owned().await else {
                    return;
                };
                let store = Arc::clone(&store);
                tokio::spawn(async move {
                    handle(stream, &store).await;
                    drop(permit);
                    tracing::debug!(target: "holosmith::keyproxy", peer = %peer, "the connection is closed");
                });
            }
            Err(error) => {
                tracing::warn!(target: "holosmith::keyproxy", %error, "the key proxy could not accept a connection");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// One connection: read a head, validate the dummy, read the body, forward, stream back.
pub async fn handle(mut stream: TcpStream, store: &Store) {
    let (head_text, leftover) = match read_head(&mut stream).await {
        Some(head) => head,
        None => {
            let _ = respond(&mut stream, 400, "the request head is not HTTP/1.1").await;
            return;
        }
    };
    let head = match parse(&head_text) {
        Ok(head) => head,
        Err(error) => {
            let _ = respond(&mut stream, error.status(), &error.sentence()).await;
            Record::refused("-", "-", "-", error.word()).log();
            return;
        }
    };
    let provider = head.route.provider;
    let secrets = match store.load() {
        Ok(secrets) => secrets,
        Err(error) => {
            let _ = respond(
                &mut stream,
                503,
                &format!("the key store could not be read: {error}"),
            )
            .await;
            Record::refused("-", provider.id, provider.host, "store").log();
            return;
        }
    };
    let Some(dummy) = head.dummy() else {
        let _ = respond(
            &mut stream,
            401,
            "this proxy needs the session's dummy key in x-api-key or Authorization: Bearer",
        )
        .await;
        Record::refused("-", provider.id, provider.host, "no-key").log();
        return;
    };
    let Some(session) = Store::session_of(&secrets, dummy) else {
        let _ = respond(
            &mut stream,
            401,
            "that dummy key is not valid for any session",
        )
        .await;
        Record::refused("-", provider.id, provider.host, "bad-key").log();
        return;
    };
    let Some(real) = secrets.providers.get(provider.id) else {
        let _ = respond(
            &mut stream,
            503,
            &format!(
                "no {} key is stored on this machine; the operator runs `holosmith key set {}`",
                provider.id, provider.id
            ),
        )
        .await;
        Record::refused(session, provider.id, provider.host, "no-provider-key").log();
        return;
    };
    if let Some(value) = head.content_length
        && value > MAX_BODY
    {
        let _ = respond(
            &mut stream,
            413,
            "the request body is larger than this proxy accepts",
        )
        .await;
        Record::refused(session, provider.id, provider.host, "too-large").log();
        return;
    }
    if head
        .header("expect")
        .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
        && stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .await
            .is_err()
    {
        return;
    }
    let body = match read_body(&mut stream, &head, leftover).await {
        Ok(body) => body,
        Err(sentence) => {
            let _ = respond(&mut stream, 400, &sentence).await;
            Record::refused(session, provider.id, provider.host, "bad-body").log();
            return;
        }
    };
    let request = upstream(&head, real, &body);
    forward(stream, &head, session, request, body.len()).await;
}

/// Forward `request` to `head.route.provider` over TLS and stream the answer back, logging one record.
async fn forward(mut client: TcpStream, head: &Head, session: &str, request: Vec<u8>, body: usize) {
    let provider = head.route.provider;
    let mut command = Command::new(OPENSSL);
    command
        .args([
            "s_client",
            "-quiet",
            "-connect",
            &format!("{}:443", provider.host),
            "-servername",
            provider.host,
            "-verify_return_error",
            "-verify_hostname",
            provider.host,
            "-CAfile",
            CA_FILE,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = respond(
                &mut client,
                502,
                &format!("the key proxy could not start its TLS client: {error}"),
            )
            .await;
            Record::refused(session, provider.id, provider.host, "no-openssl").log();
            return;
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = respond(&mut client, 502, "the TLS client has no stdin").await;
        Record::refused(session, provider.id, provider.host, "no-openssl").log();
        return;
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = respond(&mut client, 502, "the TLS client has no stdout").await;
        Record::refused(session, provider.id, provider.host, "no-openssl").log();
        return;
    };
    if let Some(mut stderr) = child.stderr.take() {
        // Drain it, or a provider that logs to stderr would block the TLS client. A tail is enough for the
        // log line a failed forward prints.
        tokio::spawn(async move {
            let mut tail = Vec::new();
            let mut buffer = [0u8; 4096];
            while let Ok(read) = stderr.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
                tail.extend_from_slice(&buffer[..read]);
                if tail.len() > 8192 {
                    let keep = tail.split_off(tail.len() - 8192);
                    tail = keep;
                }
            }
            if !tail.is_empty() {
                tracing::debug!(
                    target: "holosmith::keyproxy",
                    tail = %String::from_utf8_lossy(&tail).trim(),
                    "the TLS client's stderr"
                );
            }
        });
    }
    if stdin.write_all(&request).await.is_err() || stdin.flush().await.is_err() {
        let _ = respond(
            &mut client,
            502,
            "the TLS client did not accept the request",
        )
        .await;
        Record::refused(session, provider.id, provider.host, "write").log();
        return;
    }
    // Keep stdin open: closing it makes `openssl s_client` send a close-notify before the response. The
    // process ends when the upstream closes (every request asks for `Connection: close`).
    let mut first_buffer = [0u8; 16 * 1024];
    let first = tokio::time::timeout(FIRST_BYTE_TIMEOUT, stdout.read(&mut first_buffer)).await;
    let (up, mut down) = (body as u64, 0u64);
    let mut status = "forwarded";
    match first {
        Ok(Ok(0)) | Err(_) => {
            let _ = child.kill().await;
            let sentence = if first.is_err() {
                "the provider did not answer before the key proxy's bound"
            } else {
                "the provider closed the connection before answering"
            };
            let _ = respond(&mut client, 502, sentence).await;
            Record::refused(session, provider.id, provider.host, "upstream").log();
            return;
        }
        Ok(Ok(read)) => {
            if client.write_all(&first_buffer[..read]).await.is_err() {
                let _ = child.kill().await;
                Record {
                    session: session.to_string(),
                    provider: provider.id,
                    host: provider.host,
                    status: "client-closed",
                    up,
                    down,
                }
                .log();
                return;
            }
            down += read as u64;
        }
        Ok(Err(_)) => {
            let _ = child.kill().await;
            let _ = respond(&mut client, 502, "the TLS client failed before answering").await;
            Record::refused(session, provider.id, provider.host, "upstream").log();
            return;
        }
    }
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        match stdout.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                if client.write_all(&buffer[..read]).await.is_err() {
                    status = "client-closed";
                    break;
                }
                down += read as u64;
            }
            Err(_) => break,
        }
    }
    let _ = child.kill().await;
    Record {
        session: session.to_string(),
        provider: provider.id,
        host: provider.host,
        status,
        up,
        down,
    }
    .log();
}

async fn read_head<R>(stream: &mut R) -> Option<(String, Vec<u8>)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = Vec::with_capacity(1024);
    let end = tokio::time::timeout(HEAD_TIMEOUT, async {
        loop {
            let mut chunk = [0u8; 1024];
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(end) = head_end(&buffer) {
                return Some(end);
            }
            if buffer.len() >= MAX_HEAD {
                return None;
            }
        }
    })
    .await
    .ok()
    .flatten()?;
    let leftover = buffer.split_off(end);
    let head = String::from_utf8(buffer).ok()?;
    Some((head, leftover))
}

/// Where the head ends, including its blank line: the first `\r\n\r\n` or `\n\n`.
fn head_end(buffer: &[u8]) -> Option<usize> {
    for (index, window) in buffer.windows(4).enumerate() {
        if window == b"\r\n\r\n" {
            return Some(index + 4);
        }
    }
    for (index, window) in buffer.windows(2).enumerate() {
        if window == b"\n\n" {
            return Some(index + 2);
        }
    }
    None
}

/// The request body, starting with whatever arrived in the same read as the head.
///
/// This is the P1 the review found: a client that writes head and body in one `send` leaves the
/// body in the head's last read, and a `read_exact` from the socket alone waits forever for bytes that are
/// already in memory. [`read_head`] now hands the leftover over, and this function seeds the body with it.
async fn read_body<R>(stream: &mut R, head: &Head, leftover: Vec<u8>) -> Result<Vec<u8>, String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = Prefixed {
        stream,
        pending: leftover,
    };
    if head.chunked {
        return read_chunked(&mut reader).await;
    }
    let Some(length) = head.content_length else {
        if !reader.pending.is_empty() {
            return Err("a request with a body and no Content-Length is refused".to_string());
        }
        return Ok(Vec::new());
    };
    if reader.pending.len() > length {
        return Err("the request body is longer than its Content-Length".to_string());
    }
    let mut body = std::mem::take(&mut reader.pending);
    let missing = length - body.len();
    let mut rest = vec![0u8; missing];
    if missing > 0 {
        reader.stream.read_exact(&mut rest).await.map_err(|error| {
            format!("the request body ended before its Content-Length ({error})")
        })?;
    }
    body.extend_from_slice(&rest);
    Ok(body)
}

/// A body reader that starts with the bytes the head's read already consumed.
struct Prefixed<'a, R> {
    stream: &'a mut R,
    pending: Vec<u8>,
}

impl<R> Prefixed<'_, R>
where
    R: tokio::io::AsyncRead + Unpin,
{
    async fn read_exact(&mut self, out: &mut [u8]) -> std::io::Result<()> {
        let taken = self.pending.len().min(out.len());
        out[..taken].copy_from_slice(&self.pending[..taken]);
        self.pending.drain(..taken);
        if taken < out.len() {
            self.stream.read_exact(&mut out[taken..]).await?;
        }
        Ok(())
    }

    async fn read_line(&mut self) -> Result<String, String> {
        let mut line = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            if self.pending.is_empty() {
                self.stream
                    .read_exact(&mut byte)
                    .await
                    .map_err(|error| format!("the request ended early ({error})"))?;
            } else {
                byte[0] = self.pending.remove(0);
            }
            if byte[0] == b'\n' {
                break;
            }
            if byte[0] != b'\r' {
                line.push(byte[0]);
            }
            if line.len() > MAX_HEAD {
                return Err("a request line is longer than this proxy accepts".to_string());
            }
        }
        String::from_utf8(line).map_err(|_| "a request line is not UTF-8".to_string())
    }
}

async fn read_chunked<R>(reader: &mut Prefixed<'_, R>) -> Result<Vec<u8>, String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut body = Vec::new();
    loop {
        let line = reader.read_line().await?;
        let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or("0").trim(), 16)
            .map_err(|_| "a chunk size was not hexadecimal".to_string())?;
        if size == 0 {
            // Trailer headers, then the blank line.
            loop {
                if reader.read_line().await?.is_empty() {
                    break;
                }
            }
            break;
        }
        if body.len() + size > MAX_BODY {
            return Err("the chunked body is larger than this proxy accepts".to_string());
        }
        let mut chunk = vec![0u8; size];
        reader
            .read_exact(&mut chunk)
            .await
            .map_err(|error| format!("a chunk ended early ({error})"))?;
        body.extend_from_slice(&chunk);
        let _ = reader.read_line().await?;
    }
    Ok(body)
}

async fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        413 => "Payload Too Large",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(target: &str) -> String {
        format!(
            "POST {target} HTTP/1.1\r\nHost: key-proxy:8080\r\nContent-Length: 2\r\nx-api-key: hs-dummy-x\r\n\r\n"
        )
    }

    #[test]
    fn the_provider_is_the_first_segment_and_the_real_path_is_under_it() {
        let route = Route::parse("/anthropic/v1/messages?beta=1").expect("a route");
        assert_eq!(route.provider.id, "anthropic");
        assert_eq!(route.path, "/v1/messages?beta=1");
        let route = Route::parse("/openai/chat/completions").expect("a route");
        assert_eq!(route.path, "/v1/chat/completions");
    }

    #[test]
    fn a_request_cannot_name_its_own_host_or_escape_the_provider_prefix() {
        assert!(matches!(
            Route::parse("http://example.com/").expect_err("no"),
            RouteError::NotAPath(_)
        ));
        assert!(matches!(
            Route::parse("/unknown/v1/messages").expect_err("no"),
            RouteError::UnknownProvider { .. }
        ));
        assert!(matches!(
            Route::parse("/anthropic/../../secret").expect_err("no"),
            RouteError::Traversal(_)
        ));
    }

    #[test]
    fn the_upstream_head_swaps_the_dummy_for_the_real_key_and_drops_hop_by_hop_headers() {
        let parsed = parse(&head("/anthropic/v1/messages")).expect("a head");
        let text = String::from_utf8(upstream(&parsed, "sk-real", b"{}")).expect("utf-8");
        assert!(text.starts_with("POST /v1/messages HTTP/1.1\r\n"));
        assert!(text.contains("Host: api.anthropic.com\r\n"), "{text}");
        assert!(text.contains("x-api-key: sk-real\r\n"), "{text}");
        assert!(text.contains("Content-Length: 2\r\n"), "{text}");
        assert!(!text.contains("hs-dummy-x"), "{text}");
        assert!(!text.contains("Host: key-proxy"), "{text}");
        assert!(!text.contains("Connection: keep-alive"), "{text}");
    }

    #[test]
    fn the_upstream_request_is_the_head_and_then_the_body() {
        // The defect the V3 run measured: `upstream` wrote a `Content-Length` from `body.len()` and
        // never appended the body, so every model call was a request the provider waited out and dropped.
        // The assertions are `ends_with` and "exactly one Content-Length", because a head-only request that
        // names the right length is what the old tests pinned.
        let parsed = parse(&head("/anthropic/v1/messages")).expect("a head");
        let body = b"{\"model\":\"x\"}";
        let request = upstream(&parsed, "sk-real", body);
        let text = String::from_utf8(request.clone()).expect("utf-8");
        assert_eq!(text.matches("Content-Length: ").count(), 1, "{text}");
        assert!(text.contains("Content-Length: 13\r\n"), "{text}");
        assert_eq!(
            request.len(),
            text.find("\r\n\r\n").expect("a blank line") + 4 + body.len()
        );
        assert!(request.ends_with(b"{\"model\":\"x\"}"), "{text}");
    }

    #[test]
    fn an_openai_route_uses_a_bearer_token_and_keeps_the_v1_prefix() {
        let parsed = parse(&head("/openai/chat/completions")).expect("a head");
        let text = String::from_utf8(upstream(&parsed, "sk-real", b"{}")).expect("utf-8");
        assert!(text.contains("Authorization: Bearer sk-real\r\n"), "{text}");
        assert!(text.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
    }

    #[tokio::test]
    async fn a_body_that_arrives_with_the_head_is_not_lost() {
        // The P1 the review found: a client that writes head and body in one segment used to leave the
        // body in the head's read and then wait forever for bytes already in memory.
        let (mut client, mut server) = tokio::io::duplex(4096);
        use tokio::io::AsyncWriteExt;
        client
            .write_all(
                b"POST /anthropic/v1/messages HTTP/1.1\r\nHost: key-proxy:8080\r\nContent-Length: 2\r\n\r\n{}",
            )
            .await
            .expect("the request is written");
        client.shutdown().await.expect("the client closes");
        let (head, leftover) = read_head(&mut server).await.expect("the head arrives");
        let parsed = parse(&head).expect("the head parses");
        let body = read_body(&mut server, &parsed, leftover)
            .await
            .expect("the coalesced body is read");
        assert_eq!(body, b"{}");
    }

    #[tokio::test]
    async fn a_chunked_body_that_arrives_with_the_head_is_not_lost() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        use tokio::io::AsyncWriteExt;
        client
            .write_all(
                b"POST /anthropic/v1/messages HTTP/1.1\r\nHost: key-proxy:8080\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n",
            )
            .await
            .expect("the request is written");
        client.shutdown().await.expect("the client closes");
        let (head, leftover) = read_head(&mut server).await.expect("the head arrives");
        let parsed = parse(&head).expect("the head parses");
        let body = read_body(&mut server, &parsed, leftover)
            .await
            .expect("the coalesced chunked body is read");
        assert_eq!(body, b"{}");
    }

    #[test]
    fn a_dummy_key_is_read_from_either_header_and_never_echoed() {
        let parsed = parse(&head("/anthropic/v1/messages")).expect("a head");
        assert_eq!(parsed.dummy(), Some("hs-dummy-x"));
        let bearer =
            "POST /openai/chat/completions HTTP/1.1\r\nAuthorization: Bearer hs-dummy-y\r\n\r\n";
        assert_eq!(parse(bearer).expect("a head").dummy(), Some("hs-dummy-y"));
    }
}
