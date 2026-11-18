//! The plaintext-to-TLS relay both credential proxies are built on.
//!
//! `holosmith proxy key` and `holosmith proxy mcp` are two rules over one transport. A session's client speaks
//! plaintext HTTP/1.1 on a session network to a container that holds a real credential, and the answer is read from
//! a fixed host over TLS and copied back as it arrives. **The rule** — which host, which path, which key and in
//! which header — belongs to each proxy ([`crate::keyproxy::Route`], [`crate::mcpproxy::Route`]). Everything here is
//! the transport, and it lives in one module so the two proxies cannot become two readings of HTTP/1.1 or two TLS
//! clients (ADR-0048, ADR-0099).
//!
//! The split is the one `egress.rs` has: [`parse`] and the request readers decide from bytes and can be tested
//! without a socket; [`serve`] and [`forward`] are the half that moves them.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

/// The longest request head a relay reads. A forwarded request is a request line, a handful of headers and possibly
/// a body; the bound keeps a client that never sends a blank line from holding a task.
pub const MAX_HEAD: usize = 64 * 1024;

/// The longest request body a relay will hold. A prompt can be large, but a value beyond this is a mistake or an
/// attempt to make the proxy the memory the container is not allowed to be.
///
/// **A request bound, not a response one.** The answer is streamed, so a body this proxy never counted — a large
/// issue description on an MCP response, an SSE stream held for a session's life — is copied as it arrives and never
/// held here.
pub const MAX_BODY: usize = 32 * 1024 * 1024;

/// How many connections one relay serves at once. Each may hold a `MAX_BODY`, so an unbounded accept loop would let
/// one session spend the memory every other session's calls need.
///
/// The count is **per process**, which is why it is a budget each proxy container spends alone: a stream held for a
/// session's life costs one of its own proxy's slots, not one of the key proxy's.
pub const MAX_CONNECTIONS: usize = 64;

/// How long a client may take to send its head.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the TLS client may take to answer its first byte. A host that never answers is a 502.
pub const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(120);

/// The CA bundle the image carrying the proxies has.
pub const CA_FILE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// The `openssl` program the relay asks for TLS. See the module docs for why this is a process.
pub const OPENSSL: &str = "openssl";

/// One parsed request head: the method, the origin-form target it names, and the headers the forward needs.
///
/// The **target is kept** rather than resolved here: which row a target names is the proxy's rule, and a relay that
/// picked one would be the request-controlled route ADR-0050 refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    /// The origin-form request target, exactly as the client spelled it (`/linear/mcp?x=1`).
    pub target: String,
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

    /// The session's dummy key, from either header a client may carry it in: `x-api-key`, which pi's
    /// `anthropic-messages` provider sends, and `Authorization: Bearer`, which its `openai-completions` provider and
    /// an MCP client's `headers` both send.
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

/// Parse a request head (`POST /linear/mcp HTTP/1.1\r\n…\r\n\r\n`), leaving the body to the caller.
///
/// `None` is a head that is not HTTP/1.1: there is no method, no target, or no `HTTP/` version. The caller answers
/// it in its own words, because the sentence says which routes *it* forwards.
pub fn parse(head: &str) -> Option<Head> {
    let mut lines = head
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line));
    let request = lines.next()?;
    let mut fields = request.split(' ').filter(|field| !field.is_empty());
    let method = fields.next().unwrap_or_default().to_string();
    let target = fields.next().unwrap_or_default().to_string();
    let version = fields.next().unwrap_or_default();
    if method.is_empty() || target.is_empty() || !version.starts_with("HTTP/") {
        return None;
    }
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
    Some(Head {
        method,
        target,
        headers,
        content_length,
        chunked,
    })
}

/// The first line of a head, for the sentence a head that is not HTTP/1.1 gets.
pub fn first_line(head: &str) -> &str {
    head.lines()
        .next()
        .unwrap_or_default()
        .trim_end_matches('\r')
}

/// The headers a forward never copies, in one list for both proxies.
///
/// Eight describe the connection to **this** proxy rather than the request's meaning (`Connection`, `Te`,
/// `Transfer-Encoding`, `Content-Length` and friends), and two carry a credential the upstream host must not see:
/// `authorization` and `x-api-key` hold the session's **dummy**, whatever shape the client used. `Host` is replaced
/// by the row's, and the real key is written by [`Upstream::request`].
///
/// Everything else is passed through, which is what an MCP request needs and a model call always needed: `Accept`,
/// `Mcp-Session-Id`, `MCP-Protocol-Version`, `Content-Type` and a provider's `anthropic-beta` are part of the
/// request's meaning, not of its transport.
pub const NOT_FORWARDED: [&str; 10] = [
    "host",
    "connection",
    "proxy-connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "content-length",
    "expect",
    "authorization",
];

/// Whether a header name is one a forward drops, at any capitalisation.
pub fn not_forwarded(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    NOT_FORWARDED.contains(&lower.as_str()) || lower == "x-api-key"
}

/// What one proxy decided to forward: the row's host, the path that row's API expects, and the header its key goes
/// in.
///
/// The two proxies' routes differ (a provider row and its **API shape**, a tool row and its endpoint), and this is
/// the one shape both of them hand the shared request builder — so the rule "the key goes in the row's header and
/// nowhere else" has one implementation rather than two.
pub struct Upstream<'a> {
    pub method: &'a str,
    pub host: &'static str,
    pub path: &'a str,
    pub auth: crate::key::Auth,
}

impl Upstream<'_> {
    /// The request that goes over TLS: the row's path, the real key in the header the row uses, the body the client
    /// sent, and only the headers that mean the same thing upstream.
    ///
    /// **The body is part of the request, not an argument to its length.** A head with a `Content-Length` and no
    /// bytes is a request the host waits out and then drops: that shape shipped once and made every model call fail
    /// with `the provider closed the connection before answering` while the unit tests passed because they
    /// asserted the header.
    pub fn request(&self, head: &Head, real_key: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!("{} {} HTTP/1.1\r\n", self.method, self.path);
        out.push_str(&format!("Host: {}\r\n", self.host));
        out.push_str("Connection: close\r\n");
        for (name, value) in &head.headers {
            if not_forwarded(name) {
                continue;
            }
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        match self.auth {
            crate::key::Auth::XApiKey => out.push_str(&format!("x-api-key: {real_key}\r\n")),
            crate::key::Auth::Bearer => {
                out.push_str(&format!("Authorization: Bearer {real_key}\r\n"));
            }
        }
        if !body.is_empty() || head.content_length.is_some() || head.chunked {
            out.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        out.push_str("\r\n");
        let mut request = out.into_bytes();
        request.extend_from_slice(body);
        request
    }
}

/// Read a request head, and whatever arrived in the same read after it.
pub async fn read_head<R>(stream: &mut R) -> Option<(String, Vec<u8>)>
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
/// A client that writes head and body in one `send` leaves the body in the head's last read, and a `read_exact`
/// from the socket alone waits forever for bytes that are already in memory. [`read_head`] hands the leftover over,
/// and this function seeds the body with it.
pub async fn read_body<R>(stream: &mut R, head: &Head, leftover: Vec<u8>) -> Result<Vec<u8>, String>
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

/// Answer a request that never reached the upstream host, in the proxy's own plaintext.
pub async fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
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

/// What the TLS half of one request did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relayed {
    /// The answer's bytes reached the client; this many of them were streamed.
    Answered(u64),
    /// The client went away partway through the answer.
    ClientClosed(u64),
    /// Nothing was answered. The caller owns the sentence the client reads and the word its log line carries.
    Failed(Failure),
}

/// Why a request never got its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// `openssl` could not be started, with the spawn's own sentence.
    NoTlsClient(String),
    /// The TLS client has no stdin or no stdout to move bytes through.
    NoPipe,
    /// The TLS client did not accept the request this relay wrote into it.
    Write,
    /// The host closed the connection without answering.
    ClosedBeforeAnswer,
    /// The host did not answer within [`FIRST_BYTE_TIMEOUT`].
    Bound,
    /// The TLS client failed before answering.
    Read,
}

impl Failure {
    /// The word a proxy's log line carries, which is this build's vocabulary for a failed forward.
    pub fn word(&self) -> &'static str {
        match self {
            Self::NoTlsClient(_) | Self::NoPipe => "no-openssl",
            Self::Write => "write",
            Self::ClosedBeforeAnswer | Self::Bound | Self::Read => "upstream",
        }
    }

    /// The sentence the client is answered with, as a 502.
    pub fn sentence(&self) -> String {
        match self {
            Self::NoTlsClient(error) => {
                format!("the proxy could not start its TLS client: {error}")
            }
            Self::NoPipe => "the TLS client has no pipe to move bytes through".to_string(),
            Self::Write => "the TLS client did not accept the request".to_string(),
            Self::ClosedBeforeAnswer => {
                "the provider closed the connection before answering".to_string()
            }
            Self::Bound => "the host did not answer before the proxy's bound".to_string(),
            Self::Read => "the TLS client failed before answering".to_string(),
        }
    }
}

/// Serve forever, one task per connection, [`MAX_CONNECTIONS`] at a time.
///
/// `kind` names the proxy in this loop's own lines for the same reason a record names it: one operator reading a
/// journal should be able to tell which of the machine's proxies accepted and could not go on.
pub async fn serve<H, F>(listener: TcpListener, kind: &'static str, handler: H)
where
    H: Fn(TcpStream) -> F + Clone + Send + 'static,
    F: std::future::Future<Output = ()> + Send,
{
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let Ok(permit) = Arc::clone(&slots).acquire_owned().await else {
                    return;
                };
                let handler = handler.clone();
                tokio::spawn(async move {
                    handler(stream).await;
                    drop(permit);
                    tracing::debug!(target: "holosmith::relay", kind, peer = %peer, "the connection is closed");
                });
            }
            Err(error) => {
                tracing::warn!(target: "holosmith::relay", kind, %error, "a proxy could not accept a connection");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Forward `request` to `host` over TLS and stream the answer back into `client`.
///
/// The TLS client is `openssl s_client` because the artifact is static and has no TLS stack of its own (ADR-0048).
/// Response bytes are copied as they arrive, which is what makes an SSE body — or an answer larger than any bound
/// this proxy holds, such as a long issue description — work at all: nothing is buffered behind a `Content-Length`
/// that never comes.
///
/// **The request asks for `Connection: close`**, so the process ends when the upstream closes and one request is one
/// TLS handshake. A streamed answer (an MCP `GET`'s event stream) is the exception in what it costs, not in how it is
/// read: the loop below reads until the host closes, so the connection is held for as long as the stream is.
pub async fn forward(client: &mut TcpStream, host: &str, request: &[u8]) -> Relayed {
    let mut command = Command::new(OPENSSL);
    command
        .args([
            "s_client",
            "-quiet",
            "-connect",
            &format!("{host}:443"),
            "-servername",
            host,
            "-verify_return_error",
            "-verify_hostname",
            host,
            "-CAfile",
            CA_FILE,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Relayed::Failed(Failure::NoTlsClient(error.to_string())),
    };
    let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill().await;
        return Relayed::Failed(Failure::NoPipe);
    };
    if let Some(mut stderr) = child.stderr.take() {
        // Drain it, or a host that logs to stderr would block the TLS client. A tail is enough for the log line a
        // failed forward prints.
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
                    target: "holosmith::relay",
                    tail = %String::from_utf8_lossy(&tail).trim(),
                    "the TLS client's stderr"
                );
            }
        });
    }
    if stdin.write_all(request).await.is_err() || stdin.flush().await.is_err() {
        let _ = child.kill().await;
        return Relayed::Failed(Failure::Write);
    }
    // Keep stdin open: closing it makes `openssl s_client` send a close-notify before the response. The process ends
    // when the upstream closes (every request asks for `Connection: close`).
    let mut first_buffer = [0u8; 16 * 1024];
    let first = tokio::time::timeout(FIRST_BYTE_TIMEOUT, stdout.read(&mut first_buffer)).await;
    let mut down = 0u64;
    match first {
        Ok(Ok(0)) | Err(_) => {
            let _ = child.kill().await;
            let failure = if first.is_err() {
                Failure::Bound
            } else {
                Failure::ClosedBeforeAnswer
            };
            return Relayed::Failed(failure);
        }
        Ok(Ok(read)) => {
            if client.write_all(&first_buffer[..read]).await.is_err() {
                let _ = child.kill().await;
                return Relayed::ClientClosed(down);
            }
            down += read as u64;
        }
        Ok(Err(_)) => {
            let _ = child.kill().await;
            return Relayed::Failed(Failure::Read);
        }
    }
    let mut buffer = vec![0u8; 16 * 1024];
    let mut closed = false;
    loop {
        match stdout.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                if client.write_all(&buffer[..read]).await.is_err() {
                    closed = true;
                    break;
                }
                down += read as u64;
            }
            Err(_) => break,
        }
    }
    let _ = child.kill().await;
    if closed {
        Relayed::ClientClosed(down)
    } else {
        Relayed::Answered(down)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn a_head_is_its_method_its_target_and_the_headers_it_carries() {
        let head = parse(
            "POST /linear/mcp HTTP/1.1\r\nHost: mcp-proxy:8090\r\nAccept: application/json, \
             text/event-stream\r\nMcp-Session-Id: abc\r\nContent-Length: 2\r\n\r\n",
        )
        .expect("a head");
        assert_eq!(head.method, "POST");
        assert_eq!(head.target, "/linear/mcp");
        assert_eq!(head.header("host"), Some("mcp-proxy:8090"));
        assert_eq!(
            head.header("mcp-session-id"),
            Some("abc"),
            "a header name is compared case-insensitively"
        );
        assert_eq!(head.content_length, Some(2));
        assert!(!head.chunked);
    }

    #[test]
    fn a_request_line_that_is_not_http_is_not_a_head() {
        assert!(parse("CONNECT example.com:443 HTTP/1.1\r\n\r\n").is_some());
        assert!(parse("GET /\r\n\r\n").is_none());
        assert!(parse("").is_none());
        assert_eq!(first_line("nonsense\r\n\r\n"), "nonsense");
    }

    #[test]
    fn a_dummy_key_is_read_from_either_header() {
        let head = parse("POST /linear/mcp HTTP/1.1\r\nAuthorization: Bearer hs-dummy-y\r\n\r\n")
            .expect("a head");
        assert_eq!(head.dummy(), Some("hs-dummy-y"));
        let head = parse("GET /mcp HTTP/1.1\r\nx-api-key: hs-dummy-x\r\n\r\n").expect("a head");
        assert_eq!(head.dummy(), Some("hs-dummy-x"));
    }

    #[test]
    fn a_hop_by_hop_or_credential_header_is_not_forwarded_and_a_request_header_is() {
        for name in [
            "Connection",
            "TE",
            "Transfer-Encoding",
            "Expect",
            "Host",
            "Authorization",
            "x-api-key",
        ] {
            assert!(not_forwarded(name), "{name}");
        }
        for name in [
            "Accept",
            "Mcp-Session-Id",
            "MCP-Protocol-Version",
            "Content-Type",
        ] {
            assert!(!not_forwarded(name), "{name}");
        }
    }

    #[test]
    fn the_forwarded_request_carries_the_body_and_the_real_key_in_the_rows_header() {
        let head = parse(
            "POST /linear/mcp HTTP/1.1\r\nHost: mcp-proxy:8090\r\nAuthorization: Bearer hs-dummy\r\n\
             Accept: application/json, text/event-stream\r\nMcp-Session-Id: s-1\r\nContent-Length: 2\r\n\r\n",
        )
        .expect("a head");
        let request = Upstream {
            method: &head.method,
            host: "mcp.linear.app",
            path: "/mcp",
            auth: crate::key::Auth::Bearer,
        }
        .request(&head, "lin_api_real", b"{}");
        let text = String::from_utf8(request.clone()).expect("utf-8");
        assert!(text.starts_with("POST /mcp HTTP/1.1\r\n"), "{text}");
        assert!(text.contains("Host: mcp.linear.app\r\n"), "{text}");
        assert!(
            text.contains("Accept: application/json, text/event-stream\r\n"),
            "{text}"
        );
        assert!(text.contains("Mcp-Session-Id: s-1\r\n"), "{text}");
        assert!(
            text.contains("Authorization: Bearer lin_api_real\r\n"),
            "{text}"
        );
        assert!(!text.contains("hs-dummy"), "{text}");
        assert!(!text.contains("x-api-key"), "{text}");
        assert_eq!(text.matches("Content-Length: ").count(), 1, "{text}");
        assert!(request.ends_with(b"{}"), "{text}");
    }

    #[tokio::test]
    async fn a_body_that_arrives_with_the_head_is_not_lost() {
        // A client that writes head and body in one segment used to leave the body in the head's read and then
        // wait forever for bytes already in memory.
        let (mut client, mut server) = tokio::io::duplex(4096);
        client
            .write_all(
                b"POST /linear/mcp HTTP/1.1\r\nHost: mcp-proxy:8090\r\nContent-Length: 2\r\n\r\n{}",
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
        client
            .write_all(
                b"POST /linear/mcp HTTP/1.1\r\nHost: mcp-proxy:8090\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n",
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
}
