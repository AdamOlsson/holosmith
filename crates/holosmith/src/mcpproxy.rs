//! `holosmith proxy mcp`: the plaintext-to-TLS proxy a tool's stored key is spent through (ADR-0099).
//!
//! A session's agent speaks the MCP streamable-HTTP protocol to a tool's host — `POST` for a call, `GET` for the
//! server's event stream, `DELETE` to end a session — and every one of those requests is authenticated by a real key.
//! That key may not live in the session: `pi-home` is inside a session mount, so a key there is a key the agent can
//! read (ADR-0098). So the request arrives here in plaintext with the session's **dummy**, and this module does what
//! the key proxy does for a model call:
//!
//! 1. **Select a tool by path.** The first segment is a row in [`crate::key::TOOLS`], and the rest of the path must be
//!    one of the endpoints that row names. Both are constants, so a request cannot name a host, a port, or a path the
//!    tool table does not carry — ADR-0045's exact-list rule, and the reason the row is read from [`crate::key`]
//!    rather than from the request's own bytes.
//! 2. **Validate the dummy key.** It is a value in `keys.toml`'s `[sessions]` table, and the session id it names is
//!    the log line's attribution.
//! 3. **Swap the tool's real key in.** The dummy is dropped with every other credential header and the row's key is
//!    written in the header the row names ([`crate::key::Auth`]) — `Authorization: Bearer` for Linear. The key is read
//!    from the mounted `keys.toml` on every request, so `key set` rotates it with no restart.
//! 4. **Forward over TLS and stream back.** [`crate::relay`] is that half, shared with the key proxy: the method and
//!    every header that is neither hop-by-hop nor a credential pass through (`Accept: application/json,
//!    text/event-stream`, `Mcp-Session-Id`, `MCP-Protocol-Version`), and the answer is copied as bytes arrive, so a
//!    held event stream works and a large issue description is never buffered.
//!
//! **Why a third container rather than a route on `hs_key`.** `hs_key` is joined to every session network
//! *unconditionally*, because a model call must work with the Internet checkbox either way, and it stands on the
//! machine's `egress` network, which has real egress. Routing a tool's host through it would therefore make that host
//! reachable with Internet access off, which contradicts ADR-0046's "the checkbox is topology" and `docs/config.md`'s
//! "off means a session reaches no Internet". So `hs_mcp` is a container of its own, joined by the **same condition
//! the egress proxy is** (`DockerBackend::apply_proxy`), and the checkbox stays the gate. ADR-0099 records the rest.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use crate::config::LinearMcp;
use crate::key::{self, Store, Tool};
use crate::relay::{self, Head, Relayed};

/// Where the proxy listens unless `--listen` says otherwise.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:8090";

/// The port the proxy listens on, and the port a session's MCP configuration names.
pub const MCP_PROXY_PORT: u16 = 8090;

/// The alias a session's container resolves this proxy by. A network alias, not the container's name, for
/// [`crate::key::KEY_PROXY_ALIAS`]'s reason: Docker's embedded DNS answers an alias only on networks the asker shares.
pub const MCP_PROXY_ALIAS: &str = "mcp-proxy";

/// The container's name. `hs_mcp`, one per machine.
pub const MCP_PROXY_NAME: &str = "hs_mcp";

/// The file pi reads a session's MCP servers from, inside the agent's own directory.
pub const CLIENT_FILE: &str = "mcp.json";

/// The name a run with this container prefix gives it: `hs_mcp`, or `smoke_hs_mcp`.
///
/// Prefixed for [`crate::container::proxy_name`]'s reason, and it is the key proxy's reason: this container mounts the
/// key store of the config it was started from, so an unprefixed smoke run's proxy would replace the machine's with one
/// holding an empty store.
pub fn mcp_proxy_name(prefix: &str) -> String {
    format!("{prefix}{MCP_PROXY_NAME}")
}

/// The `mcp.json` a session's pi-home carries: one `linear` server on this proxy, with the session's dummy key.
///
/// `Ok(None)` is [`LinearMcp::Off`] and only it, which is the whole of "off writes no file": the caller removes what
/// an earlier start left rather than writing an entry a session cannot use (ADR-0100). `Ok(Some(text))` is a
/// configured mode's document. `Err` is a configured mode the tool table cannot answer, in a sentence naming the mode
/// and what the row was missing.
///
/// The two answers are a `Result` rather than one `Option` because the caller reads the absence of a document as "the
/// key says off" and removes the file: a `write` whose row stopped naming its endpoint would then configure nothing
/// and report success, which is the one outcome this file's design exists to prevent.
///
/// The url is composed from [`MCP_PROXY_ALIAS`], [`MCP_PROXY_PORT`] and the endpoint [`endpoint`] reads out of
/// [`key::tool`]'s `linear` row — the same three values [`Route::parse`] matches a request against, so a client this
/// build configures is a route this build serves. `Authorization` carries the session's **dummy**, because this file
/// sits inside a session mount and a real key there is a key the agent can read (ADR-0098).
///
/// `exposure: "deferred"` is about the size of Linear's tool set rather than a preference: a deferred server is one
/// the model reaches with a `tool_search` call instead of one whose every tool is declared in every prompt, and
/// `description` is the sentence that lists it until then.
pub fn client_config(mode: LinearMcp, dummy: &str) -> Result<Option<String>, String> {
    // Answered before the row is read, so a build that stopped carrying `linear` still answers the default mode.
    if mode == LinearMcp::Off {
        return Ok(None);
    }
    let tool = key::tool("linear").ok_or_else(|| {
        format!(
            "a session cannot be configured with Linear's {} tools: this build has no linear tool row",
            mode.as_str()
        )
    })?;
    let endpoint = endpoint(tool.endpoints, mode)?;
    let document = serde_json::json!({
        "mcpServers": {
            (tool.id): {
                "url": format!("http://{MCP_PROXY_ALIAS}:{MCP_PROXY_PORT}/{}{endpoint}", tool.id),
                "headers": {"Authorization": format!("Bearer {dummy}")},
                "exposure": "deferred",
                "timeout": 30,
                "description": description(mode),
            }
        }
    });
    let mut text = serde_json::to_string_pretty(&document).map_err(|error| {
        format!(
            "a session's Linear {} configuration could not be rendered as JSON ({error})",
            mode.as_str()
        )
    })?;
    text.push('\n');
    Ok(Some(text))
}

/// The endpoint `mode` reaches, chosen out of a tool row's endpoints.
///
/// The read-only endpoint is the row's one whose last segment is the mode's own word, and the full one is the row's
/// other — so a changed endpoint in [`key::TOOLS`] cannot leave a session's file pointing at a path the proxy refuses.
/// `Err` names the mode and the endpoint the row did not have, because a row that cannot answer is a mode whose
/// configuration would otherwise be missing and read as `off` ([`client_config`]).
///
/// A slice rather than [`Tool`], and a function of its own, so the failing arm is a test's to drive: [`key::TOOLS`] is
/// a `const`, so no test can hand this a `linear` row that lacks an endpoint.
fn endpoint<'a>(endpoints: &'a [&'a str], mode: LinearMcp) -> Result<&'a str, String> {
    let readonly = endpoints
        .iter()
        .copied()
        .find(|path| path.rsplit('/').next() == Some(LinearMcp::ReadOnly.as_str()));
    match mode {
        LinearMcp::ReadOnly => readonly.ok_or_else(|| {
            format!(
                "a session cannot be configured with Linear's {} tools: the linear tool row names no endpoint \
                 ending in /{}",
                mode.as_str(),
                LinearMcp::ReadOnly.as_str()
            )
        }),
        LinearMcp::Write => endpoints
            .iter()
            .copied()
            .find(|path| Some(*path) != readonly)
            .ok_or_else(|| {
                format!(
                    "a session cannot be configured with Linear's {} tools: the linear tool row names no \
                     endpoint other than /{}",
                    mode.as_str(),
                    LinearMcp::ReadOnly.as_str()
                )
            }),
        // `client_config` answers `off` with the absence of a document before it asks for an endpoint, so no caller
        // prints this; the arm answers rather than panicking because a mode is a value, not a programming error.
        LinearMcp::Off => Err(format!(
            "{} names no endpoint: a session with no Linear configuration has no mcp.json at all",
            mode.as_str()
        )),
    }
}

/// The sentence that lists a configured server, which is what `exposure: "deferred"` leaves the model until it asks
/// for the tools themselves.
fn description(mode: LinearMcp) -> &'static str {
    match mode {
        LinearMcp::ReadOnly => {
            "Linear's issue tracker, read-only: issues, projects and cycles can be read and not changed"
        }
        LinearMcp::Write => {
            "Linear's issue tracker: read and write this workspace's issues, projects and cycles"
        }
        // `off` configures no server, so it has no sentence to list one with.
        LinearMcp::Off => "",
    }
}

/// One request selected by its path, before the key is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub tool: &'static Tool,
    /// The path the tool's host serves the endpoint under, with the proxy's `/<tool>` prefix removed. It is always one
    /// of [`Tool::endpoints`](crate::key::Tool::endpoints), never a path this proxy composed.
    pub path: String,
}

impl Route {
    /// Parse an origin-form request target (`/linear/mcp/readonly`).
    ///
    /// The tool segment is matched exactly against [`key::TOOLS`], and the rest of the path exactly against that
    /// row's [`endpoints`](Tool::endpoints). A path that does not start with `/`, a tool this build does not hold a
    /// key for, an endpoint the row does not name, or a `..` segment is refused with a sentence: there is no fallback
    /// host or path to send it to, and the whole selection comes from the table rather than from the request.
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
            return Err(RouteError::NoTool(target.to_string()));
        }
        let tool = key::tool(id).ok_or_else(|| RouteError::UnknownTool {
            tool: id.to_string(),
        })?;
        let tail = segments.next().unwrap_or_default();
        let endpoint = format!("/{tail}");
        if endpoint.split('/').any(|segment| segment == "..") {
            return Err(RouteError::Traversal(target.to_string()));
        }
        if !tool.endpoints.contains(&endpoint.as_str()) {
            return Err(RouteError::UnknownEndpoint {
                tool: tool.id,
                endpoint,
            });
        }
        let path = match query {
            Some(query) => format!("{endpoint}?{query}"),
            None => endpoint,
        };
        Ok(Self { tool, path })
    }
}

/// Why a request target is not a route this build can forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    NotAPath(String),
    NoTool(String),
    UnknownTool {
        tool: String,
    },
    UnknownEndpoint {
        tool: &'static str,
        endpoint: String,
    },
    Traversal(String),
}

impl RouteError {
    pub fn status(&self) -> u16 {
        match self {
            Self::UnknownTool { .. } | Self::UnknownEndpoint { .. } => 403,
            Self::NotAPath(_) | Self::NoTool(_) | Self::Traversal(_) => 400,
        }
    }

    pub fn word(&self) -> &'static str {
        match self {
            Self::NotAPath(_) => "not-a-path",
            Self::NoTool(_) => "no-tool",
            Self::UnknownTool { .. } => "unknown-tool",
            Self::UnknownEndpoint { .. } => "unknown-endpoint",
            Self::Traversal(_) => "traversal",
        }
    }

    pub fn sentence(&self) -> String {
        match self {
            Self::NotAPath(target) => format!(
                "{target:?} is not an origin-form path: send /<tool>/<endpoint> to the MCP proxy"
            ),
            Self::NoTool(target) => {
                format!("{target:?} has no tool segment: send /<tool>/<endpoint> to the MCP proxy")
            }
            Self::UnknownTool { tool } => format!(
                "\"{tool}\" is not a tool this build holds a key for ({}), so no key was used",
                key::tool_ids().join(", ")
            ),
            Self::UnknownEndpoint { tool, endpoint } => format!(
                "\"{endpoint}\" is not an endpoint `{tool}` serves ({}), so no key was used",
                key::tool(tool)
                    .map_or_else(Vec::new, |row| row.endpoints.to_vec())
                    .join(", ")
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

/// One parsed request: the transport's head and this proxy's route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub head: Head,
    pub route: Route,
}

impl Request {
    /// The session's dummy key, from either header a client may carry it in.
    pub fn dummy(&self) -> Option<&str> {
        self.head.dummy()
    }
}

/// Parse a request head, then its route: the two refusals a request can get before a key is read.
pub fn parse(head: &str) -> Result<Request, RouteError> {
    let head = relay::parse(head)
        .ok_or_else(|| RouteError::NotAPath(relay::first_line(head).to_string()))?;
    let route = Route::parse(&head.target)?;
    Ok(Request { head, route })
}

/// Build the request that goes over TLS: the tool's path, the real key in the header the row names, the body the
/// client sent, and the method and headers an MCP request is made of.
///
/// The bytes are [`relay::Upstream::request`]'s. What matters here is what it keeps: `Accept` (which a streamable-HTTP
/// client sets to `application/json, text/event-stream`), `Mcp-Session-Id`, `MCP-Protocol-Version`, `Content-Type` and
/// the method — `POST`, `GET` and `DELETE` alike. The dummy and any `x-api-key` the client sent are dropped, because
/// the row's header is the only one that carries a credential.
pub fn upstream(request: &Request, real_key: &str, body: &[u8]) -> Vec<u8> {
    relay::Upstream {
        method: &request.head.method,
        host: request.route.tool.host,
        path: &request.route.path,
        auth: request.route.tool.auth,
    }
    .request(&request.head, real_key, body)
}

/// The one request a probe sends: the uninitialised `tools/list` call, which a streamable-HTTP server answers
/// without a session and without spending a tool.
///
/// It is the body `scripts/verify.sh`'s `case_mcp_proxy_request` sends, deliberately: the probe's request and the
/// one the gate reads are the same call, so a `doctor` that passes and a session that works are one measurement.
pub const PROBE_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

/// The protocol version **this build's probe** announces to a tool's host.
///
/// A session's client negotiates its own `MCP-Protocol-Version` and the proxy passes it through unchanged (see
/// [`upstream`]); this constant is only what a probe with no client of its own says, so the two must not be confused.
/// A host that refuses this version answers `400`, which [`crate::doctor`] reads as "not a verdict about the key" —
/// so a host that moves on can stop the check measuring a good key, and the line names the status that answered
/// rather than claiming the key is bad (ADR-0101).
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// How long a probe's whole answer may take.
///
/// A forward's first byte waits [`relay::FIRST_BYTE_TIMEOUT`] (120 s) and its stream waits on nothing, because a
/// session's answer may be an event stream held for that session's life. A probe is the other caller: one
/// `tools/list` call, read to its end by whatever command an operator — or a deploy's health step — is blocked on. So
/// this bound is short enough that a host which accepted the connection and then went quiet cannot hold an install,
/// and generous enough for a TLS handshake plus a status line from a host having a slow minute.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// The sentence a session's tool call and `doctor --full` both carry when this machine holds no key for `tool`.
///
/// One spelling, because "no key is stored" is one fact about the machine whichever end of it is asked: a session
/// gets it as a 503 and an operator gets it as a `tool-key: unknown` line (ADR-0101).
pub fn no_key_sentence(tool: &str) -> String {
    format!("no {tool} key is stored on this machine; the operator runs `holosmith key set {tool}`")
}

/// The request bytes a probe sends: `POST` to one of the row's endpoints, the row's host, the headers an MCP
/// request is made of, and the stored key in the header the row names.
///
/// The head and the body are [`relay::Upstream::request`]'s, over a [`Head`] this function composes, rather than
/// hand-spelled: a probe that wrote its own request would be a second reading of what this build sends, and the one
/// thing a probe has to prove is that the key this machine stores is the key this proxy spends (ADR-0099).
///
/// The endpoint is the caller's, and it is always one of [`Tool::endpoints`](crate::key::Tool::endpoints) — no
/// argument here can name a host or a path the tool table does not carry (ADR-0045). The version it announces is
/// [`MCP_PROTOCOL_VERSION`], which is this probe's rather than a session's.
pub fn probe_request(tool: &Tool, endpoint: &'static str, key: &str) -> Vec<u8> {
    let body = PROBE_BODY.as_bytes();
    let head = Head {
        method: "POST".to_string(),
        target: endpoint.to_string(),
        headers: vec![
            (
                "Accept".to_string(),
                "application/json, text/event-stream".to_string(),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "MCP-Protocol-Version".to_string(),
                MCP_PROTOCOL_VERSION.to_string(),
            ),
        ],
        content_length: Some(body.len()),
        chunked: false,
    };
    relay::Upstream {
        method: "POST",
        host: tool.host,
        path: endpoint,
        auth: tool.auth,
    }
    .request(&head, key, body)
}

/// What one probe learned about a stored key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// The host's first line named this status, for this endpoint.
    Status { status: u16, endpoint: &'static str },
    /// Nothing measured the key, in the sentence saying why: a row with no endpoint, a TLS client that could not
    /// start, a host that closed or did not answer within [`PROBE_TIMEOUT`], or an answer that is not
    /// HTTP.
    Unmeasured(String),
}

/// Ask `tool`'s host whether `key` still answers: one `tools/list` call, and its status line.
///
/// The verdicts belong to the caller, because what a status means for a rotation is `doctor`'s rule: this function
/// only says whether an HTTP status came back, and [`crate::doctor`] reads 2xx, 401/403 and the rest differently.
pub async fn probe(tool: &Tool, key: &str) -> Probe {
    let Some(endpoint) = tool.endpoints.first().copied() else {
        return Probe::Unmeasured(format!(
            "this build's {} tool row names no endpoint to probe",
            tool.id
        ));
    };
    let request = probe_request(tool, endpoint, key);
    let answer = match relay::request(tool.host, &request, PROBE_TIMEOUT).await {
        Ok(answer) => answer,
        Err(failure) => return Probe::Unmeasured(failure.sentence()),
    };
    let text = String::from_utf8_lossy(&answer);
    match status_of(&text) {
        Some(status) => Probe::Status { status, endpoint },
        None => Probe::Unmeasured(format!(
            "{} answered {:?}, which is not an HTTP status line",
            tool.host,
            relay::first_line(&text)
        )),
    }
}

/// The status code out of an answer's first line (`HTTP/1.1 200 OK`), `None` when the first line is not one.
fn status_of(answer: &str) -> Option<u16> {
    let mut fields = relay::first_line(answer).split(' ');
    if !fields.next()?.starts_with("HTTP/") {
        return None;
    }
    fields.next()?.parse::<u16>().ok()
}

/// What one answered request logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub session: String,
    pub tool: &'static str,
    pub host: &'static str,
    pub status: &'static str,
    pub up: u64,
    pub down: u64,
}

impl Record {
    pub fn refused(
        session: &str,
        tool: &'static str,
        host: &'static str,
        status: &'static str,
    ) -> Self {
        Self {
            session: session.to_string(),
            tool,
            host,
            status,
            up: 0,
            down: 0,
        }
    }

    /// One line per request, at `info`: a tool call is a fact an operator may need to attribute to a session.
    pub fn log(&self) {
        tracing::info!(
            target: "holosmith::mcpproxy",
            session = %self.session,
            tool = %self.tool,
            host = %self.host,
            status = %self.status,
            bytes_up = self.up,
            bytes_down = self.down,
            "the MCP proxy answered a request"
        );
    }
}

/// Serve forever, one task per connection.
pub async fn serve(listener: TcpListener, store: Arc<Store>) {
    relay::serve(listener, "MCP proxy", move |stream| {
        let store = Arc::clone(&store);
        async move { handle(stream, &store).await }
    })
    .await
}

/// One connection: read a head, validate the dummy, read the body, forward, stream back.
pub async fn handle(mut stream: TcpStream, store: &Store) {
    let (head_text, leftover) = match relay::read_head(&mut stream).await {
        Some(head) => head,
        None => {
            let _ = relay::respond(&mut stream, 400, "the request head is not HTTP/1.1").await;
            return;
        }
    };
    let request = match parse(&head_text) {
        Ok(request) => request,
        Err(error) => {
            let _ = relay::respond(&mut stream, error.status(), &error.sentence()).await;
            Record::refused("-", "-", "-", error.word()).log();
            return;
        }
    };
    let tool = request.route.tool;
    let secrets = match store.load() {
        Ok(secrets) => secrets,
        Err(error) => {
            let _ = relay::respond(
                &mut stream,
                503,
                &format!("the key store could not be read: {error}"),
            )
            .await;
            Record::refused("-", tool.id, tool.host, "store").log();
            return;
        }
    };
    let Some(dummy) = request.dummy() else {
        let _ = relay::respond(
            &mut stream,
            401,
            "this proxy needs the session's dummy key in x-api-key or Authorization: Bearer",
        )
        .await;
        Record::refused("-", tool.id, tool.host, "no-key").log();
        return;
    };
    let Some(session) = Store::session_of(&secrets, dummy) else {
        let _ = relay::respond(
            &mut stream,
            401,
            "that dummy key is not valid for any session",
        )
        .await;
        Record::refused("-", tool.id, tool.host, "bad-key").log();
        return;
    };
    let Some(real) = Store::tool_key(&secrets, tool.id) else {
        let _ = relay::respond(&mut stream, 503, &no_key_sentence(tool.id)).await;
        Record::refused(session, tool.id, tool.host, "no-tool-key").log();
        return;
    };
    if let Some(value) = request.head.content_length
        && value > relay::MAX_BODY
    {
        let _ = relay::respond(
            &mut stream,
            413,
            "the request body is larger than this proxy accepts",
        )
        .await;
        Record::refused(session, tool.id, tool.host, "too-large").log();
        return;
    }
    if request
        .head
        .header("expect")
        .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
        && stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .await
            .is_err()
    {
        return;
    }
    let body = match relay::read_body(&mut stream, &request.head, leftover).await {
        Ok(body) => body,
        Err(sentence) => {
            let _ = relay::respond(&mut stream, 400, &sentence).await;
            Record::refused(session, tool.id, tool.host, "bad-body").log();
            return;
        }
    };
    let answered = upstream(&request, real, &body);
    forward(
        &mut stream,
        session,
        tool.id,
        tool.host,
        &answered,
        body.len(),
    )
    .await;
}

/// Forward `request` to the tool's host over TLS and stream the answer back, logging one record.
///
/// A `GET` here is the server's event stream, and it is held for as long as the server keeps it: the relay reads
/// until the host closes, so this connection and one `openssl` child are spent for that life rather than for one call.
/// That is one of [`relay::MAX_CONNECTIONS`] slots in *this* process — not the key proxy's, which is another
/// container with its own.
async fn forward(
    stream: &mut TcpStream,
    session: &str,
    tool: &'static str,
    host: &'static str,
    request: &[u8],
    body: usize,
) {
    match relay::forward(stream, host, request).await {
        Relayed::Answered(down) => Record {
            session: session.to_string(),
            tool,
            host,
            status: "forwarded",
            up: body as u64,
            down,
        }
        .log(),
        Relayed::ClientClosed(down) => Record {
            session: session.to_string(),
            tool,
            host,
            status: "client-closed",
            up: body as u64,
            down,
        }
        .log(),
        Relayed::Failed(failure) => {
            let _ = relay::respond(stream, 502, &failure.sentence()).await;
            Record::refused(session, tool, host, failure.word()).log();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::Secrets;

    fn head(method: &str, target: &str) -> String {
        format!(
            "{method} {target} HTTP/1.1\r\nHost: mcp-proxy:8090\r\nAuthorization: Bearer hs-dummy-mcp\r\n\
             Accept: application/json, text/event-stream\r\nMcp-Session-Id: 3f1c\r\n\
             MCP-Protocol-Version: 2025-06-18\r\nContent-Length: 2\r\n\r\n"
        )
    }

    /// The two endpoints the tool table names, and nothing else: the forwarded path is the endpoint the row carries,
    /// so a route cannot be a path this proxy composed.
    #[test]
    fn the_tool_and_the_endpoint_are_the_two_segments_and_both_come_from_the_table() {
        let route = Route::parse("/linear/mcp").expect("a route");
        assert_eq!(route.tool.id, "linear");
        assert_eq!(route.tool.host, "mcp.linear.app");
        assert_eq!(route.path, "/mcp");

        let route = Route::parse("/linear/mcp/readonly").expect("a route");
        assert_eq!(route.path, "/mcp/readonly");

        let route = Route::parse("/linear/mcp?session=1").expect("a route");
        assert_eq!(route.path, "/mcp?session=1");
    }

    #[test]
    fn a_request_cannot_name_a_host_a_tool_or_an_endpoint_the_table_does_not() {
        assert!(matches!(
            Route::parse("https://mcp.linear.app/mcp").expect_err("no"),
            RouteError::NotAPath(_)
        ));
        assert!(matches!(
            Route::parse("/not-a-tool/mcp").expect_err("no"),
            RouteError::UnknownTool { .. }
        ));
        assert!(matches!(
            Route::parse("/linear/sse").expect_err("no"),
            RouteError::UnknownEndpoint { .. }
        ));
        assert!(matches!(
            Route::parse("/linear/../../v1/messages").expect_err("no"),
            RouteError::Traversal(_)
        ));
        for error in [
            Route::parse("/not-a-tool/mcp").expect_err("no"),
            Route::parse("/linear/sse").expect_err("no"),
        ] {
            assert_eq!(error.status(), 403, "{error}");
            assert!(error.sentence().contains("no key was used"), "{error}");
        }
    }

    /// The relay's promise, read out of the MCP route: the method, the session header, the protocol version and the
    /// `Accept` a streamable-HTTP client sets all reach the host, and the credential does not.
    #[test]
    fn the_method_the_mcp_headers_and_the_body_pass_through_and_the_dummy_does_not() {
        for method in ["POST", "GET", "DELETE"] {
            let parsed = parse(&head(method, "/linear/mcp")).expect("a head");
            let request = upstream(&parsed, "lin_api_real", b"{}");
            let text = String::from_utf8(request.clone()).expect("utf-8");
            assert!(
                text.starts_with(&format!("{method} /mcp HTTP/1.1\r\n")),
                "{text}"
            );
            assert!(text.contains("Host: mcp.linear.app\r\n"), "{text}");
            assert!(
                text.contains("Accept: application/json, text/event-stream\r\n"),
                "{text}"
            );
            assert!(text.contains("Mcp-Session-Id: 3f1c\r\n"), "{text}");
            assert!(
                text.contains("MCP-Protocol-Version: 2025-06-18\r\n"),
                "{text}"
            );
            assert!(
                text.contains("Authorization: Bearer lin_api_real\r\n"),
                "{text}"
            );
            assert!(!text.contains("hs-dummy"), "{text}");
            assert!(!text.contains("x-api-key"), "{text}");
            assert_eq!(
                text.matches("lin_api_real").count(),
                1,
                "the key must appear once, in one header: {text}"
            );
            assert!(request.ends_with(b"{}"), "{text}");
        }
    }

    /// The readonly endpoint is a second path on the same row, and a Bearer row writes no `x-api-key` even when the
    /// client sent one: a client that carries both must not hand the host a stale value.
    #[test]
    fn a_bearer_row_writes_no_x_api_key_even_when_the_client_sent_one() {
        let parsed =
            parse("DELETE /linear/mcp/readonly HTTP/1.1\r\nx-api-key: hs-dummy-mcp\r\n\r\n")
                .expect("a head");
        let text = String::from_utf8(upstream(&parsed, "lin_api_real", b"")).expect("utf-8");
        assert!(
            text.starts_with("DELETE /mcp/readonly HTTP/1.1\r\n"),
            "{text}"
        );
        assert!(
            text.contains("Authorization: Bearer lin_api_real\r\n"),
            "{text}"
        );
        assert!(!text.contains("x-api-key"), "{text}");
        assert!(
            !text.contains("Content-Length"),
            "a request with no body names no length: {text}"
        );
    }

    /// The dummy is read from `Authorization` (an MCP client's `headers`) or from `x-api-key`, so a client written
    /// against pi's own provider config works here too.
    #[test]
    fn the_dummy_is_read_from_either_header() {
        let parsed = parse(&head("POST", "/linear/mcp")).expect("a head");
        assert_eq!(parsed.dummy(), Some("hs-dummy-mcp"));
        let parsed = parse("POST /linear/mcp HTTP/1.1\r\nx-api-key: hs-dummy-other\r\n\r\n")
            .expect("a head");
        assert_eq!(parsed.dummy(), Some("hs-dummy-other"));
    }

    /// The document a configured mode writes, for a test that is not about either failure arm.
    fn configured(mode: LinearMcp, dummy: &str) -> String {
        client_config(mode, dummy)
            .expect("the table's row can answer both configured modes")
            .expect("a configured mode writes a document")
    }

    /// The session's own configuration, read back as JSON: the entry is one server on this proxy, the two modes differ
    /// only in the endpoint they name, and both endpoints come from the tool table.
    #[test]
    fn the_client_configuration_names_one_server_on_the_tables_two_endpoints() {
        let tool = key::tool("linear").expect("the table names linear");
        let write = entry(&configured(LinearMcp::Write, "hs-dummy-mcp"));
        let readonly = entry(&configured(LinearMcp::ReadOnly, "hs-dummy-mcp"));
        assert_eq!(write["url"], "http://mcp-proxy:8090/linear/mcp");
        assert_eq!(readonly["url"], "http://mcp-proxy:8090/linear/mcp/readonly");
        // The endpoints are the table's, not a second spelling: each url is the alias, the port, the tool id and one
        // of the row's paths.
        for endpoint in tool.endpoints {
            let url = format!(
                "http://{MCP_PROXY_ALIAS}:{MCP_PROXY_PORT}/{}{endpoint}",
                tool.id
            );
            assert!(
                write["url"] == url.as_str() || readonly["url"] == url.as_str(),
                "{endpoint} is not what either mode configured: {write} {readonly}"
            );
        }
        assert_ne!(write["url"], readonly["url"]);
        assert_eq!(
            write["url"]
                .as_str()
                .and_then(|url| url.strip_suffix("/mcp")),
            readonly
                .get("url")
                .and_then(|url| url.as_str())
                .and_then(|url| url.strip_suffix("/mcp/readonly")),
            "the two modes must differ only in the endpoint they name"
        );
        assert_eq!(write["headers"]["Authorization"], "Bearer hs-dummy-mcp");
        for entry in [&write, &readonly] {
            assert_eq!(entry["exposure"], "deferred");
            assert_eq!(entry["timeout"], 30);
            assert!(
                entry["description"]
                    .as_str()
                    .is_some_and(|description| !description.is_empty()),
                "every configured server carries a sentence: {entry}"
            );
        }
        assert!(
            readonly["description"]
                .as_str()
                .is_some_and(|sentence| sentence.contains("not changed")),
            "the read-only server must say it cannot write: {readonly}"
        );
        for mode in [LinearMcp::Write, LinearMcp::ReadOnly] {
            let text = configured(mode, "hs-dummy-mcp");
            assert!(
                text.ends_with('\n'),
                "the file ends with a newline: {text:?}"
            );
            assert!(
                text.contains("hs-dummy-mcp"),
                "the dummy is missing: {text}"
            );
            let secrets = Secrets {
                tools: std::collections::BTreeMap::from([(
                    "linear".to_string(),
                    "lin_api_the_real_key".to_string(),
                )]),
                ..Secrets::default()
            };
            assert!(
                !key::contains_real_key(&secrets, &text),
                "a real tool key reached the session's configuration: {text}"
            );
        }
        assert_eq!(
            client_config(LinearMcp::Off, "hs-dummy-mcp"),
            Ok(None),
            "off is the one mode that writes no file at all"
        );
    }

    /// The endpoint is the row's, and a row that cannot answer its mode is a sentence rather than a document that
    /// configures nothing: `off` is what `Ok(None)` stands for, so the failure arm is driven through a slice — the tool
    /// table is a `const`, and no test can write a `linear` row that lacks an endpoint.
    #[test]
    fn the_endpoint_comes_from_the_row_and_a_row_that_cannot_answer_is_a_sentence() {
        let row = key::tool("linear").expect("the table names linear");
        assert_eq!(
            endpoint(row.endpoints, LinearMcp::ReadOnly),
            Ok("/mcp/readonly")
        );
        assert_eq!(endpoint(row.endpoints, LinearMcp::Write), Ok("/mcp"));

        let no_readonly =
            endpoint(&["/mcp"], LinearMcp::ReadOnly).expect_err("there is no read-only endpoint");
        assert!(
            no_readonly.contains("readonly"),
            "the sentence names the mode it could not configure: {no_readonly}"
        );
        let empty = endpoint(&[], LinearMcp::ReadOnly).expect_err("the row names no endpoint");
        assert!(
            empty.contains("readonly"),
            "an empty row is the same sentence: {empty}"
        );

        let only_readonly = endpoint(&["/mcp/readonly"], LinearMcp::Write)
            .expect_err("the read-only endpoint is not the full one");
        assert!(
            only_readonly.contains("write"),
            "the write mode is named too: {only_readonly}"
        );
        let empty = endpoint(&[], LinearMcp::Write).expect_err("the row names no endpoint");
        assert!(
            empty.contains("write"),
            "an empty row is the same sentence: {empty}"
        );
    }

    /// The probe's bytes are the proxy's bytes: the row's endpoint and host, the headers an MCP request is made of,
    /// the stored key in the row's header, and the `tools/list` body — composed by the same builder a session's call
    /// goes through, so a `doctor` pass is evidence about the path a tool call takes (ADR-0101).
    #[test]
    fn the_probe_is_the_same_request_a_session_sends_with_the_stored_key() {
        let row = key::tool("linear").expect("the table names linear");
        let endpoint = row.endpoints.first().copied().expect("the row names one");
        let request = probe_request(row, endpoint, "lin_api_rotated");
        let text = String::from_utf8(request.clone()).expect("utf-8");
        assert_eq!(
            text.lines().next(),
            Some(format!("POST {endpoint} HTTP/1.1").as_str()),
            "{text}"
        );
        assert!(text.contains("Host: mcp.linear.app\r\n"), "{text}");
        assert!(
            text.contains("Accept: application/json, text/event-stream\r\n"),
            "{text}"
        );
        assert!(
            text.contains("Content-Type: application/json\r\n"),
            "{text}"
        );
        assert!(
            text.contains(&format!("MCP-Protocol-Version: {MCP_PROTOCOL_VERSION}\r\n")),
            "{text}"
        );
        assert!(text.contains("Connection: close\r\n"), "{text}");
        assert!(
            text.contains("Authorization: Bearer lin_api_rotated\r\n"),
            "{text}"
        );
        assert_eq!(
            text.matches("lin_api_rotated").count(),
            1,
            "the key appears once, in one header: {text}"
        );
        assert_eq!(
            text.matches("Content-Length: ").count(),
            1,
            "one length, from the body's real size: {text}"
        );
        assert!(
            text.contains(&format!("Content-Length: {}", PROBE_BODY.len())),
            "{text}"
        );
        assert!(request.ends_with(PROBE_BODY.as_bytes()), "{text}");
        assert!(
            text.contains("\"method\":\"tools/list\""),
            "the probe asks for the tool list, which is a call every streamable-HTTP server answers: {text}"
        );
    }

    /// A row with no endpoint is a sentence rather than a probe, and every status line shape a host may answer is
    /// read exactly once: the intro `HTTP/1.1 200 OK`, a status with no reason, and two answers that are not HTTP.
    #[tokio::test]
    async fn a_row_with_no_endpoint_is_not_measured_and_a_status_line_is_read_as_its_code() {
        const NO_ENDPOINT: Tool = Tool {
            id: "no-endpoint",
            host: "example.invalid",
            auth: key::Auth::Bearer,
            endpoints: &[],
        };
        match probe(&NO_ENDPOINT, "a-key").await {
            Probe::Unmeasured(sentence) => assert!(
                sentence.contains("no endpoint"),
                "the sentence names what the row lacked: {sentence}"
            ),
            other => panic!("a row with no endpoint probed something: {other:?}"),
        }

        assert_eq!(status_of("HTTP/1.1 200 OK\r\n\r\n"), Some(200));
        assert_eq!(status_of("HTTP/1.1 401 Unauthorized\r\n\r\n"), Some(401));
        assert_eq!(status_of("HTTP/1.1 503\r\n..."), Some(503));
        assert_eq!(status_of("<html>not this\r\n"), None);
        assert_eq!(status_of("HTTP/1.1 not-a-number\r\n"), None);
        assert_eq!(status_of(""), None);
    }

    /// One server entry out of a client document, so a test reads the file pi reads.
    fn entry(text: &str) -> serde_json::Value {
        let document: serde_json::Value =
            serde_json::from_str(text).expect("the client configuration is JSON");
        let servers = &document["mcpServers"];
        let entry = servers["linear"].clone();
        assert_eq!(
            servers.as_object().map(serde_json::Map::len),
            Some(1),
            "a session is configured with one server: {text}"
        );
        entry
    }

    #[test]
    fn the_container_and_the_alias_are_the_third_of_each() {
        assert_eq!(MCP_PROXY_NAME, "hs_mcp");
        assert_eq!(mcp_proxy_name(""), "hs_mcp");
        assert_eq!(mcp_proxy_name("smoke_"), "smoke_hs_mcp");
        assert_eq!(MCP_PROXY_ALIAS, "mcp-proxy");
        assert_eq!(MCP_PROXY_PORT, 8090);
        assert_eq!(
            DEFAULT_LISTEN,
            format!("0.0.0.0:{MCP_PROXY_PORT}"),
            "the listen address and the port a session's configuration names must be one value"
        );
    }
}
