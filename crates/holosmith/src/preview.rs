//! The preview listener: a session's own dev server, on its own origin (HS-041, spec item 3, V7).
//!
//! Item 3 asks for a **second listener** that serves only `/<host>/<id>/*` (HTTP and WebSocket), has no API
//! routes, checks an upgrade's `Origin`, and uses `local` as the host name of this machine's own sessions.
//! This module is that listener, and the shape it takes is the binary's third **byte tunnel** — after
//! [`crate::egress`] and [`crate::keyproxy`] — rather than a second router:
//!
//! * What it carries is not its business. A dev server emits chunked bodies, SSE, and `101 Switching
//!   Protocols`; a proxy that *forwards* the bytes cannot be wrong about any of them, while one that
//!   re-frames a body through its own HTTP stack has to decode chunked encoding to re-encode it — the exact
//!   class of bug a tunnel cannot have. The `101` a browser sees is the upstream's own.
//! * `axum` 0.8 here has no `ws` feature, and forwarding an HTTP exchange through it would need hyper's
//!   `client` feature, which pulls `want` into `Cargo.lock`: two crates for one feature (ADR-0051).
//! * It cannot have an API route at all, because it has no route table. The acceptance's "no API routes" is
//!   a property of the construction, not a list that has to be kept empty by hand.
//!
//! V7 was measured before this was written: the host **can** fetch from a container on an `--internal`
//! network (`http://172.18.0.2:8099/` answered `200` from the host while the container reached neither the
//! internet nor the host, ADR-0044's pair). So the spec's documented fallback — the preview proxy running
//! as a container on the session network — is **not** implemented: this listener connects to the address
//! the container has on its own network ([`Containers`]).
//!
//! The split is the one [`crate::egress`] and [`crate::selftest`] have: [`Route::strip`], [`parse_head`] and
//! [`rewrite`] decide from values and are tested without a socket, [`Upstream`] is the seam that lets a test
//! point the listener at a stub server, and [`handle`] only moves bytes through a decision it was given.

use std::fmt;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use crate::config::Config;
use crate::db::Database;
use crate::docker::Docker;
use crate::tunnel::{self, Tunnel};

/// The host name a server uses for its own sessions (spec item 3, GLOSSARY's **Local**).
///
/// Every other name is another machine's, which the hub forwards (HS-069). Keeping the segment a **value**
/// is what makes that story a forwarder rather than a second parser: [`Route::strip`] already answers which
/// host a path names.
pub const HOST: &str = "local";

/// The longest request head this listener reads. A browser's request is a few hundred bytes; the bound is
/// what keeps a client that never sends a blank line from holding a task.
pub const MAX_HEAD: usize = 16 * 1024;

/// How long a client may take to send its request head.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// How long connecting to a session's dev server may take.
///
/// Short, unlike the egress proxy's ten seconds: this address is a container on a Docker network the host
/// can see, so a connection either answers at once or the container is not there.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest pause between two failed `accept`s (the escalation [`crate::egress`] uses, for its reason).
pub const ACCEPT_BACKOFF: Duration = Duration::from_secs(5);

/// The deadline the drain gets on a stop signal: the control listener's own (ADR-0007's ten seconds).
pub const DRAIN: Duration = crate::serve::DEFAULT_SHUTDOWN_GRACE;

/// The host name and the session id of one preview request, and what the upstream is asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The first path segment: `local` on this machine (HS-069 forwards the rest).
    pub host: String,
    /// The second: a session id.
    pub id: String,
    /// Everything after the id, as the upstream sees it: `/`, `/index.html`, `/api/x?y=1`.
    pub rest: String,
    /// Whether the client named the session's **root** without its trailing slash: `/local/<id>` but not
    /// `/local/<id>/` and not `/local/<id>/anything`.
    ///
    /// Both spellings reach the upstream as `/`, but a **page** served from the unslashed one has
    /// `/local/<id>` as its base URL, so its relative `./assets/x.js` resolves to `/local/assets/x.js` — a
    /// different host segment, and a 404. Only that shape is redirected, and only a browser is: an upgrade
    /// is not a page.
    pub slashless_root: bool,
}

impl Route {
    /// Split `/<host>/<id>/<rest>` into its three parts, refusing only what is not that shape.
    ///
    /// The segments are **not** percent-decoded: this is a tunnel, so a path reaches the dev server the way
    /// the browser spelled it. The query string is kept (a dev server's `?route=` is its own business), and a
    /// path with no trailing slash is the upstream's root, which is why `/local/<id>` and `/local/<id>/` are
    /// the same request.
    ///
    /// **Whose host it is, is not this function's decision** (HS-069): a hub serves another machine's sessions
    /// under that machine's registered name, so the host segment is a *value* the caller resolves against the
    /// registry ([`plan`]) rather than a rule this splitter can apply. Nor is the id's alphabet one: a peer's
    /// session is none of this machine's business, and the local arm answers "no such session" from its own table
    /// for a name that could not be one ([`Context::holds`]).
    pub fn strip(path: &str) -> Result<Self, Refusal> {
        let (path, query) = match path.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (path, None),
        };
        let mut segments = path.splitn(4, '/');
        let not_a_path = Refusal::NotAPath {
            path: path.to_string(),
        };
        if segments.next() != Some("") {
            return Err(not_a_path);
        }
        let host = segments.next().filter(|host| !host.is_empty());
        let id = segments.next().filter(|id| !id.is_empty());
        let (Some(host), Some(id)) = (host, id) else {
            return Err(not_a_path);
        };
        let tail = segments.next().unwrap_or("");
        let mut rest = format!("/{tail}");
        if let Some(query) = query {
            rest.push('?');
            rest.push_str(query);
        }
        Ok(Self {
            host: host.to_string(),
            id: id.to_string(),
            rest,
            slashless_root: tail.is_empty() && !path.ends_with('/'),
        })
    }

    /// The path a browser navigates to for `id` on `origin`: what the session screen links to.
    pub fn path(id: &str) -> String {
        format!("/{HOST}/{id}/")
    }

    /// The slashed path this route would be at, keeping the query: where a slashless request is sent.
    pub fn slashed_path(&self) -> String {
        let mut target = format!("/{}/{}/", self.host, self.id);
        if let Some((_, query)) = self.rest.split_once('?') {
            target.push('?');
            target.push_str(query);
        }
        target
    }
}

/// What this listener answers a request it will not proxy, and why.
///
/// Every variant carries the words of its own answer, because the two listeners have different contracts:
/// the control listener answers ADR-0008's envelope, and this one answers a **sentence** in plain text — a
/// preview is a page an operator opens on a phone, and an envelope here would be the API surface item 3
/// says this listener does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The request line is not a path this listener serves.
    NotAPath { path: String },
    /// The first segment names no host this machine's registry holds — so it is not a preview this machine serves,
    /// on any role (HS-069: a name that *is* a row is forwarded, and a machine that forwards nothing says so with
    /// [`Refusal::NotForwarded`] instead).
    UnknownHost(String),
    /// The second segment is not a session id, or no row holds it.
    UnknownSession(String),
    /// The session has no running container, which is every session on a stub run and a stopped one here.
    NoContainer { id: String, detail: String },
    /// Nothing answered at the container's address.
    Unreachable {
        id: String,
        address: SocketAddr,
        detail: String,
    },
    /// The question "is this a session?" could not be answered at all.
    Unreadable { detail: String },
    /// An upgrade whose `Origin` is missing or is not [`Context::origin`] (spec item 3).
    ForeignOrigin {
        expected: String,
        sent: Option<String>,
    },
    /// This machine's role forwards nothing, so another machine's name is not a preview it can reach (HS-069).
    NotForwarded { host: String },
    /// The machine that asked is not one this machine takes calls from (HS-070, spec item 4, V11).
    ///
    /// One variant for both of the trust check's refusals, because this listener's own vocabulary is one sentence in
    /// plain text: [`crate::trust::Refusal`] owns the words (a peer and the node the tailnet named for it, never an
    /// `allowed_hubs` entry) and this variant owns the fact that a *preview* listener said them. The code differs per
    /// arm, so the log line a test greps is the one a reader expects.
    Peer { refusal: crate::trust::Refusal },
    /// The registry knows the host but not where its previews answer: no probe has carried a `preview_origin` from
    /// it yet (HS-069, ADR-0074).
    NoPreview { host: String },
    /// A hub dialled the host's preview origin and got nothing, or could not dial it at all (HS-069).
    PeerUnreachable {
        host: String,
        origin: String,
        detail: String,
    },
}

impl Refusal {
    /// The status, the code and the sentence, from **one** match.
    ///
    /// One rather than three, for the reason the control listener keeps one constructor (`ApiError::new`,
    /// ADR-0008): a new variant is then one edit, and there is no way for a variant to have a status and no
    /// sentence. The two accessors below are what a caller reads out of this.
    fn parts(&self) -> (u16, &'static str, String) {
        match self {
            Self::NotAPath { path } => (
                // "Not Found" for a path, a host or a session: the three are one answer to a browser ("there is
                // nothing here") and three sentences to a reader.
                404,
                "preview_not_a_path",
                format!(
                    "{path} is not a preview path: this listener serves /{HOST}/<session>/<path> only"
                ),
            ),
            // One sentence for both readers: `/api/health` is `/<host>/<id>` to this listener, so a control API
            // path lands here too, and claiming `api` is a machine would be a sentence that is wrong about the
            // thing it names. What is true either way is where previews live and where another machine's do.
            Self::UnknownHost(host) => (
                404,
                "preview_unknown_host",
                format!(
                    "this machine serves its own sessions as /{HOST}/<session>/<path>, and {host} is not a host \
                     in this machine's registry"
                ),
            ),
            Self::UnknownSession(id) => (
                404,
                "preview_unknown_session",
                format!("no session is called {id}"),
            ),
            Self::NoContainer { id, detail } => (
                502,
                "preview_no_container",
                format!("session {id} has no running container, so it has no dev server: {detail}"),
            ),
            Self::Unreachable {
                id,
                address,
                detail,
            } => (
                502,
                "preview_unreachable",
                format!("session {id} answered nothing at {address}: {detail}"),
            ),
            Self::Unreadable { detail } => (
                500,
                "preview_unreadable",
                format!("the session could not be read: {detail}"),
            ),
            Self::NotForwarded { host } => (
                403,
                "preview_not_forwarded",
                format!(
                    "this machine's role runs no forwarding, so {host} is another machine's name and only a hub \
                     reaches it; this machine serves its own sessions as /{HOST}/<session>/<path>"
                ),
            ),
            Self::Peer { refusal } => (
                refusal.status(),
                match refusal {
                    crate::trust::Refusal::NotListed { .. } => "preview_peer_not_allowed",
                    crate::trust::Refusal::NoPeer { .. }
                    | crate::trust::Refusal::Unverifiable { .. } => "preview_peer_unverifiable",
                },
                refusal.sentence(),
            ),
            Self::NoPreview { host } => (
                502,
                "preview_no_peer_preview",
                format!(
                    "the registry knows {host} but not where it serves previews: its health has not answered \
                     with a preview origin since this machine started, so there is nothing to forward to"
                ),
            ),
            Self::PeerUnreachable {
                host,
                origin,
                detail,
            } => (
                502,
                "preview_peer_unreachable",
                format!("{host}'s preview listener could not be reached at {origin}: {detail}"),
            ),
            Self::ForeignOrigin { expected, sent } => (
                403,
                "preview_foreign_origin",
                match sent {
                    Some(sent) => format!(
                        "a WebSocket upgrade must carry the preview origin, which is {expected}, not {sent}"
                    ),
                    None => format!(
                        "a WebSocket upgrade must carry the preview origin, {expected}, and this one carried \
                         none"
                    ),
                },
            ),
        }
    }

    /// The status line this refusal answers with.
    pub fn status(&self) -> u16 {
        self.parts().0
    }

    /// The word a log line or a test names this refusal by. Not part of any body: nothing on this listener
    /// emits ADR-0008's envelope.
    pub fn code(&self) -> &'static str {
        self.parts().1
    }

    /// The one sentence the browser reads, which names the fix where there is one.
    pub fn sentence(&self) -> String {
        self.parts().2
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sentence())
    }
}

/// The three facts of a request head this listener decides on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// The method, as sent. Nothing this listener does depends on it: a dev server's form posts and its
    /// `PUT`s are as much its own business as its `GET`s.
    pub method: String,
    /// The request target: a path, with its query.
    pub target: String,
    /// The `Origin` header, when the client sent exactly one.
    pub origin: Option<String>,
    /// Whether this is a protocol upgrade — the one case item 3 makes a rule about.
    pub upgrade: bool,
}

/// Read a request line and the two headers that matter out of a head.
///
/// `None` for a head without a request line, which is the only thing that makes it unreadable: headers this
/// listener does not look at are the upstream's, and a head with no `Origin` is an ordinary request.
pub fn parse_head(head: &str) -> Option<Head> {
    let mut lines = head.lines();
    let request = lines.next()?;
    let mut parts = request.split(' ');
    let method = parts.next().filter(|method| !method.is_empty())?;
    let target = parts.next().filter(|target| target.starts_with('/'))?;
    let _version = parts.next()?;
    let mut origin = None;
    let mut origins = 0;
    let mut upgrade = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "origin" if !value.is_empty() => {
                // Two `Origin` headers have no defensible winner (the guard's rule, ADR-0010): neither does, and
                // a third does not either — which is why this counts rather than asks whether one was stored.
                origins += 1;
                if origins == 1 {
                    origin = Some(value.to_string());
                } else {
                    origin = None;
                }
            }
            // Any `Upgrade` is an upgrade. Item 3 names WebSocket because that is what a dev build uses, and
            // a different protocol must get the same `Origin` rule rather than being told to close.
            "upgrade" if !value.is_empty() => upgrade = true,
            _ => {}
        }
    }
    Some(Head {
        method: method.to_string(),
        target: target.to_string(),
        origin,
        upgrade,
    })
}

/// The head this listener sends the upstream: one request, its prefix stripped.
///
/// What is rewritten is only what a proxy must: the target becomes [`Route::rest`], `Host` becomes the
/// container's own address (a dev server's host allow-list judges that header, and the browser's
/// `localhost:8081` is not a name the container has), and a request that is not an upgrade is told
/// `Connection: close` — one client request is one upstream connection, which is what makes relaying
/// "until the other side closes" a complete answer. **Nothing else is touched**, and in particular nothing
/// in the response: a dev build's absolute URLs, its `Location` headers and its cookies still name the
/// origin root, so an app that serves its assets from `/` works and one that hard-codes a base path needs its
/// own configuration. That limit is item 3's shape, recorded in ADR-0051.
pub fn rewrite(head: &str, target: &Target<'_>, upgrade: bool) -> String {
    let mut out = String::with_capacity(head.len() + 64);
    let mut request_line = true;
    let mut wrote_host = false;
    let mut wrote_connection = false;
    let mut wrote_origin = false;
    for line in head.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // The blank line ends the head, and a body can arrive in the same read; `split_head` is what keeps it
        // out of here, so a line with no `:` after the request line is not a header and is dropped.
        if line.is_empty() {
            continue;
        }
        if request_line {
            request_line = false;
            let mut parts = line.split(' ');
            let method = parts.next().unwrap_or("GET");
            let version = parts.nth(1).unwrap_or("HTTP/1.1");
            out.push_str(&format!("{method} {} {version}\r\n", target.target));
            continue;
        }
        let Some((name, _value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "host" => {
                wrote_host = true;
                out.push_str(&format!("Host: {}\r\n", target.host));
            }
            // The **hub is the author of a forwarded request** (HS-069): a peer's listener compares an upgrade's
            // `Origin` against the preview origin it serves, and the browser's is this machine's. A local request
            // keeps the client's origin (`Target::origin` is `None`): the container is the upstream, and what the
            // browser said about itself is still true.
            "origin" if target.origin.is_some() => {
                wrote_origin = true;
                if let Some(origin) = target.origin {
                    out.push_str(&format!("Origin: {origin}\r\n"));
                }
            }
            // An upgrade keeps the client's own `Connection`, because the handshake is the upstream's to
            // accept or refuse; everything else is one request per connection.
            "connection" if !upgrade => {
                wrote_connection = true;
                out.push_str("Connection: close\r\n");
            }
            "keep-alive" | "proxy-connection" => {}
            _ => {
                out.push_str(line);
                out.push_str("\r\n");
            }
        }
    }
    if !wrote_host {
        out.push_str(&format!("Host: {}\r\n", target.host));
    }
    if let Some(origin) = target.origin.filter(|_| !wrote_origin) {
        out.push_str(&format!("Origin: {origin}\r\n"));
    }
    if !upgrade && !wrote_connection {
        out.push_str("Connection: close\r\n");
    }
    out.push_str("\r\n");
    out
}

/// What a proxied request's head must become: the target to ask for, the `Host` to name, and the `Origin` to claim.
///
/// One value rather than three parameters because the three travel together and two callers build them: the local
/// arm (`Host` is the container's address, the target is the path below the prefix, the origin is the browser's) and
/// a hub's peer arm (`Host` and `Origin` are the peer's preview origin, the target is the peer's own spelling).
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    /// What `Host` becomes. A dev server's host allow-list judges it, and a peer's listener answers nothing for a
    /// name it does not serve.
    pub host: &'a str,
    /// The request target the upstream is asked for.
    pub target: &'a str,
    /// What `Origin` becomes, or `None` to leave the client's alone.
    pub origin: Option<&'a str>,
}

/// The head and whatever arrived behind it.
///
/// [`tunnel::read_head`] reads in chunks, so a request's body can arrive with its head. The head is where the
/// blank line is; the rest is the client's bytes and is relayed to the upstream untouched, in order, before
/// the tunnel starts.
pub fn split_head(text: &str) -> (&str, &str) {
    for (marker, end) in [("\r\n\r\n", 4), ("\n\n", 2)] {
        if let Some(index) = text.find(marker) {
            let at = index + end;
            return (&text[..at], &text[at..]);
        }
    }
    (text, "")
}

/// Whether an `Origin` header (or its absence) is the preview origin (spec item 3).
///
/// Absent is **not** accepted: item 3 says an upgrade's origin *is* the preview origin, and a missing header
/// is not a match. The comparison is the guard's — trimmed, ASCII-case-insensitive, exact — so
/// `http://localhost:8081.evil.example` is a different origin and is refused (ADR-0010's rule, applied here
/// because it is the same question about the same kind of value).
pub fn from_preview_origin(preview_origin: &crate::config::Origin, origin: Option<&str>) -> bool {
    origin.is_some_and(|origin| origin.trim().eq_ignore_ascii_case(preview_origin.as_str()))
}

/// Where a session's dev server is: the one question the listener cannot answer for itself.
///
/// A trait rather than a function so the wire tests can point the listener at a stub server on a loopback
/// port — the seam `crate::selftest::Runner` and `crate::preflight::Tools` have, for the same reason.
pub trait Upstream: Send + Sync {
    /// The socket `session`'s dev server answers on, or why it cannot be asked.
    fn address(&self, session: &str) -> Result<SocketAddr, Refusal>;
}

/// The real upstream: the container's address on the session's own network (spec item 2's topology).
///
/// One `docker inspect` answers both questions — whether the container runs and what address it has there —
/// because they are one fact about one container: a stopped session has no address, and an address from a
/// network the container has left is not one. `docker inspect` is the only place a container's real
/// addresses are visible (the Docker landmine).
pub struct Containers {
    pub docker: Docker,
    /// The port a session's dev server listens on inside its container (the config's key).
    pub port: u16,
    /// This run's **raw** `network_prefix` (empty on a normal machine, `smoke_` in a smoke run, HS-077), because the
    /// inspect call names the network a container joined and [`crate::container::network_name`] is what composes it.
    pub network_prefix: String,
    /// This run's raw `container_prefix`, for the same reason: the container it inspects is that session's, and
    /// [`crate::container::container_name`] is the one function that names it.
    pub container_prefix: String,
}

/// The inspect format, with `NETWORK` standing in for the session's own network name.
///
/// A placeholder rather than a `format!` template because Docker's format is mostly braces, and four of them
/// escaped per call is a line no reader can check.
const FORMAT: &str = "{{.State.Running}} {{\
with index .NetworkSettings.Networks \"NETWORK\"}}{{.IPAddress}}{{end}}";

impl Upstream for Containers {
    fn address(&self, session: &str) -> Result<SocketAddr, Refusal> {
        let name = crate::container::container_name(&self.container_prefix, session);
        let format = FORMAT.replace(
            "NETWORK",
            &crate::container::network_name(&self.network_prefix, session),
        );
        let said = self
            .docker
            .inspect(&name, &format)
            .map_err(|error| Refusal::NoContainer {
                id: session.to_string(),
                detail: error.to_string(),
            })?;
        let mut fields = said.split_whitespace();
        let running = fields.next().unwrap_or("false");
        if running != "true" {
            return Err(Refusal::NoContainer {
                id: session.to_string(),
                detail: "it is not running".to_string(),
            });
        }
        let address = fields.next().unwrap_or("");
        let Ok(ip) = address.parse::<IpAddr>() else {
            return Err(Refusal::NoContainer {
                id: session.to_string(),
                detail: format!(
                    "it has no address on its own network (docker inspect said {:?})",
                    address
                ),
            });
        };
        Ok(SocketAddr::new(ip, self.port))
    }
}

/// What every connection needs: the origin an upgrade must come from, the rows, the role, the upstream, and the one
/// program a peer hop runs.
pub struct Context {
    /// The origin a browser reaches this listener at, and the only `Origin` an upgrade may carry (item 3).
    pub origin: crate::config::Origin,
    /// The rows, so a path naming nothing is a `404` rather than a failed connection.
    pub database: Arc<Mutex<Database>>,
    /// Which parts of the system this machine runs: `Roles` gives preview forwarding to a `hub` and a `both`, and a
    /// `server` answers another machine's name as one it does not serve (HS-069).
    pub role: crate::config::Role,
    pub upstream: Arc<dyn Upstream>,
    /// The programs a peer hop runs. Only `openssl` today, for a `https` peer's preview origin — the same child the
    /// host probe and the forwards route use (ADR-0071/ADR-0073/ADR-0074).
    pub tools: crate::preflight::Tools,
    /// Which machines may drive this listener (HS-070): the same decision the control listener asks, spelled in
    /// this listener's own words (`Refusal::Peer`).
    pub trust: crate::trust::Trust,
}

impl Context {
    /// The context a boot builds from the config in force and the programs the run uses.
    ///
    /// `tools` is a parameter rather than `Tools::default()` for the reason it is one in `serve`: the guards and this
    /// listener must resolve the trust check from the **same** value, so a machine (or a test) that names its
    /// `tailscale` names it once. Both callers pass the run's own.
    pub fn of(
        config: &Config,
        database: Arc<Mutex<Database>>,
        upstream: Arc<dyn Upstream>,
        tools: &crate::preflight::Tools,
    ) -> Self {
        Self {
            origin: config.preview_origin.clone(),
            role: config.role,
            database,
            upstream,
            trust: crate::trust::Trust::of(config, tools),
            tools: tools.clone(),
        }
    }

    /// Whether the machine that opened this connection may drive this listener at all (HS-070).
    ///
    /// The whois is a child, so it runs on the blocking pool: an accept loop must keep accepting while one peer is
    /// being verified. A blocking task that died is still a machine this listener verified nobody for, and
    /// [`crate::trust::Refusal::NoPeer`] is exactly that fact.
    async fn admits(
        &self,
        peer: Option<IpAddr>,
    ) -> Result<crate::trust::Admitted, crate::trust::Refusal> {
        let trust = self.trust.clone();
        match tokio::task::spawn_blocking(move || trust.admit(peer)).await {
            Ok(answer) => answer,
            Err(error) => Err(crate::trust::Refusal::NoPeer {
                detail: error.to_string(),
            }),
        }
    }

    /// Whether a head's `Origin` is the preview origin (see [`from_preview_origin`]).
    pub fn from_preview_origin(&self, origin: Option<&str>) -> bool {
        from_preview_origin(&self.origin, origin)
    }

    /// What this listener does with one parsed path: serve this machine's session, or relay to another machine's.
    ///
    /// One registry read, under one lock scope, and only for a host that is not [`HOST`]: `local` never reads the
    /// table, which is what makes this machine's own previews independent of its registry.
    fn plan(&self, route: &Route) -> Result<Plan, Refusal> {
        if route.host == HOST {
            return Ok(Plan::Local);
        }
        let row = {
            let database = self
                .database
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            database
                .hosts()
                .one(&route.host)
                .map_err(|error| Refusal::Unreadable {
                    detail: error.to_string(),
                })?
        };
        plan(route.host.as_str(), self.role, row.as_ref())
    }

    /// Whether the rows hold this session.
    fn holds(&self, session: &str) -> Result<bool, Refusal> {
        let database = self
            .database
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        database
            .sessions()
            .get(session)
            .map(|row| row.is_some())
            .map_err(|error| Refusal::Unreadable {
                detail: error.to_string(),
            })
    }
}

/// What this listener does with one path, as a value: the decision [`Route::strip`] deliberately does not make.
///
/// A pure table, so the four arms are testable without a socket, a database or a daemon — the shape this file's
/// other decisions have (`from_preview_origin`, `parse_head`), and the reason [`plan`] takes a row rather than
/// reading one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// This machine's own session: resolve its container and tunnel to it (spec item 3's `local`).
    Local,
    /// Another machine's session: relay the exchange to that machine's preview listener (HS-069).
    Peer(crate::config::Origin),
}

/// The four rules, in the order the answers differ.
///
/// 1. **`local` is this machine**, always and on every role — the one name item 3 spells this machine's sessions
///    with, and the one a registry row may not take (ADR-0073).
/// 2. **The name must be a row.** Anything else is [`Refusal::UnknownHost`], which is what a browser that opened an
///    old link sees — on every role, so a `server` answers an unknown name and a known one differently.
/// 3. **Only a machine that forwards forwards.** `Roles` gives request and preview forwarding to a `hub`; a
///    `server`'s answer to a name it *does* hold is that it does not serve it rather than that it is unknown.
/// 4. **The row must know where that machine's previews answer** (ADR-0074): a peer's preview origin is a second
///    listener on a second port, only the peer can say what it is, and until its health has answered with one there
///    is nothing to dial. The refusal names the probe rather than guessing a port.
pub fn plan(
    host: &str,
    role: crate::config::Role,
    row: Option<&crate::hosts::Host>,
) -> Result<Plan, Refusal> {
    if host == HOST {
        return Ok(Plan::Local);
    }
    // The name first, the role second: an unknown name is the same answer on every machine (it is a fact about the
    // registry), while "this machine forwards nothing" is only meaningful about a machine that could have.
    let Some(row) = row else {
        return Err(Refusal::UnknownHost(host.to_string()));
    };
    if role == crate::config::Role::Server {
        return Err(Refusal::NotForwarded {
            host: host.to_string(),
        });
    }
    match row.preview.clone() {
        Some(preview) => Ok(Plan::Peer(preview)),
        None => Err(Refusal::NoPreview {
            host: host.to_string(),
        }),
    }
}

/// How many connections are open, and a signal for "the last one closed".
///
/// A gauge and a `Notify` rather than a `JoinSet`: tasks are spawned detached so a preview that outlives its
/// listener is not held in the accept loop, and the drain only has to know the count reached zero. The
/// `Notify` future is created **before** the count is read, which is what makes the pair race-free.
#[derive(Default)]
struct Open {
    count: AtomicUsize,
    closed: tokio::sync::Notify,
}

impl Open {
    fn enter(&self) {
        self.count.fetch_add(1, Ordering::AcqRel);
    }

    fn leave(&self) {
        if self.count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.closed.notify_waiters();
        }
    }

    async fn wait(&self) {
        loop {
            let closed = self.closed.notified();
            if self.count.load(Ordering::Acquire) == 0 {
                return;
            }
            closed.await;
        }
    }
}

/// Serve previews on `listener` until `stop` resolves, then let open connections finish within [`DRAIN`].
///
/// The same contract as [`crate::serve::run`]: a stop signal stops accepting, in-flight work gets the grace
/// period, and a drain that runs out is one `warn` rather than an error — the process is on its way out and
/// a preview is not worth waiting for. A preview that is still streaming when the deadline passes is cut when
/// the process exits; nothing here closes its socket, because a listener cannot.
pub async fn serve<F>(listener: TcpListener, context: Arc<Context>, stop: F)
where
    F: Future<Output = ()> + Send,
{
    let open = Arc::new(Open::default());
    tokio::pin!(stop);
    let mut failures: u32 = 0;
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    failures = 0;
                    let context = Arc::clone(&context);
                    let open = Arc::clone(&open);
                    open.enter();
                    tokio::spawn(async move {
                        handle(stream, &context).await;
                        open.leave();
                        tracing::debug!(target: "holosmith::preview", peer = %peer, "the connection is closed");
                    });
                }
                Err(error) => {
                    // The escalation `crate::egress` uses, for its reason: a transient accept failure is
                    // retried at once and a listener that is gone must not spin a core filling the log.
                    failures = failures.saturating_add(1);
                    let pause = ACCEPT_BACKOFF.min(Duration::from_millis(100) * failures.min(60));
                    tracing::warn!(
                        target: "holosmith::preview",
                        %error,
                        failures,
                        pause_ms = pause.as_millis(),
                        "the preview listener could not accept a connection"
                    );
                    tokio::select! {
                        () = tokio::time::sleep(pause) => {}
                        () = &mut stop => break,
                    }
                }
            },
            () = &mut stop => break,
        }
    }
    if tokio::time::timeout(DRAIN, open.wait()).await.is_err() {
        tracing::warn!(
            target: "holosmith::preview",
            open = open.count.load(Ordering::Relaxed),
            seconds = DRAIN.as_secs(),
            "the preview listener's drain deadline was reached; its open previews are cut when the process exits"
        );
    }
}

/// One preview: read a head, decide, connect, relay the rewritten head, and tunnel until both sides close.
///
/// Nothing is retried and nothing is buffered beyond the head: a refusal is written once and the connection
/// closed, and a body of any size (a dev build's upload, a build's streaming response) moves 16 KiB at a
/// time through [`tunnel::pump_both`].
pub async fn handle(mut stream: TcpStream, context: &Context) {
    // The address the connection came from, before anything is read: it is what machine-to-machine trust asks the
    // Tailscale daemon about (HS-070). `peer_addr` fails on a socket that has already been shut down, and that is a
    // machine whose identity cannot be read — which the check refuses rather than waves through.
    let caller = stream.peer_addr().ok().map(|address| address.ip());
    let head = match tunnel::read_head(&mut stream, MAX_HEAD, HEAD_TIMEOUT).await {
        Some(head) => head,
        None => {
            let _ = respond(&mut stream, 400, NOT_A_HEAD).await;
            return;
        }
    };
    let (head, early) = split_head(&head);
    let Some(parsed) = parse_head(head) else {
        let _ = respond(&mut stream, 400, NOT_A_HEAD).await;
        return;
    };
    // Item 4's machine rule, **before** this listener's own: whether this machine takes calls from the caller at all
    // precedes whether the caller's `Origin` is one an upgrade may carry, and it is the same decision the control
    // listener makes — so a stranger's preview is refused in the words `crate::trust` owns (HS-070).
    match context.admits(caller).await {
        Ok(admitted) => admitted.log(),
        Err(refusal) => {
            refusal.log();
            let refused = Refusal::Peer { refusal };
            refuse(&mut stream, &refused, &parsed).await;
            return;
        }
    }
    let route = match Route::strip(&parsed.target) {
        Ok(route) => route,
        Err(refusal) => {
            refuse(&mut stream, &refusal, &parsed).await;
            return;
        }
    };
    // Item 3's one rule, checked before anything is asked of the machine or of a peer: an upgrade from anywhere but
    // **this** listener's preview origin never reaches a dev server. On a hub's forwarded request the origin is the
    // hub's, and the peer applies the same rule again with its own (HS-069, ADR-0074).
    if parsed.upgrade && !context.from_preview_origin(parsed.origin.as_deref()) {
        let refusal = Refusal::ForeignOrigin {
            expected: context.origin.as_str().to_string(),
            sent: parsed.origin.clone(),
        };
        refuse(&mut stream, &refusal, &parsed).await;
        return;
    }
    let plan = match context.plan(&route) {
        Ok(plan) => plan,
        Err(refusal) => {
            refuse(&mut stream, &refusal, &parsed).await;
            return;
        }
    };
    // A page asked for without its trailing slash is sent to the slashed path, because that is the base URL its
    // own relative URLs resolve against (`Route::slashed_path`). An upgrade is not a page, so it is left alone: a
    // redirected handshake is not a handshake. The redirect names the host the **client** used, so a page opened at
    // `/workshop/<id>` stays on the hub's own origin and is never bounced to another machine.
    if route.slashless_root && !parsed.upgrade {
        let target = route.slashed_path();
        let _ = redirect_to(&mut stream, &target).await;
        tracing::debug!(
            target: "holosmith::preview",
            path = %parsed.target,
            location = %target,
            "preview redirected to its slashed path"
        );
        return;
    }
    match plan {
        Plan::Local => local(stream, context, &route, &parsed, head, early).await,
        Plan::Peer(origin) => peer(stream, &route, &origin, &parsed, head, early, context).await,
    }
}

/// This machine's own session: the container's address, then the tunnel (spec item 3's `local`, V7).
async fn local(
    mut stream: TcpStream,
    context: &Context,
    route: &Route,
    parsed: &Head,
    head: &str,
    early: &str,
) {
    match context.holds(&route.id) {
        Ok(true) => {}
        Ok(false) => {
            let refusal = Refusal::UnknownSession(route.id.clone());
            refuse(&mut stream, &refusal, parsed).await;
            return;
        }
        Err(refusal) => {
            refuse(&mut stream, &refusal, parsed).await;
            return;
        }
    }
    let address = {
        let upstream = Arc::clone(&context.upstream);
        let id = route.id.clone();
        // `docker inspect` is a subprocess: it runs off the runtime's worker so other previews keep being
        // accepted while this one waits for the daemon (the rule `crate::backend` follows for every call).
        match tokio::task::spawn_blocking(move || upstream.address(&id)).await {
            Ok(Ok(address)) => address,
            Ok(Err(refusal)) => {
                refuse(&mut stream, &refusal, parsed).await;
                return;
            }
            Err(error) => {
                let refusal = Refusal::NoContainer {
                    id: route.id.clone(),
                    detail: error.to_string(),
                };
                refuse(&mut stream, &refusal, parsed).await;
                return;
            }
        }
    };
    let mut upstream =
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(upstream)) => upstream,
            Ok(Err(error)) => {
                let refusal = Refusal::Unreachable {
                    id: route.id.clone(),
                    address,
                    detail: error.to_string(),
                };
                refuse(&mut stream, &refusal, parsed).await;
                return;
            }
            Err(_) => {
                let refusal = Refusal::Unreachable {
                    id: route.id.clone(),
                    address,
                    detail: format!(
                        "the connection did not open within {} seconds",
                        CONNECT_TIMEOUT.as_secs()
                    ),
                };
                refuse(&mut stream, &refusal, parsed).await;
                return;
            }
        };
    let rewritten = rewrite(
        head,
        &Target {
            host: &address.to_string(),
            target: &route.rest,
            origin: None,
        },
        parsed.upgrade,
    );
    if let Err(error) = upstream.write_all(rewritten.as_bytes()).await {
        let refusal = Refusal::Unreachable {
            id: route.id.clone(),
            address,
            detail: error.to_string(),
        };
        refuse(&mut stream, &refusal, parsed).await;
        return;
    }
    // The bytes that arrived with the head are the client's, so they go upstream before the tunnel starts.
    if !early.is_empty() && upstream.write_all(early.as_bytes()).await.is_err() {
        return;
    }
    let (up, down) = tunnel::pump_both(stream, upstream, Tunnel::Preview).await;
    tracing::debug!(
        target: "holosmith::preview",
        session = %route.id,
        path = %route.rest,
        upgrade = parsed.upgrade,
        status = "proxied",
        up,
        down,
        "preview"
    );
}

/// Another machine's session: the same exchange, relayed to that machine's **preview listener** (HS-069).
///
/// The request is re-authored, not merely copied: the first path segment becomes [`HOST`] — the peer's own name for
/// its sessions, which is what its listener would otherwise refuse — and `Host` and `Origin` become the peer's
/// preview origin, because its listener answers `Host` with nothing and compares an upgrade's `Origin` against the
/// origin it serves. Everything else is the client's, and the tunnel cannot be wrong about the body.
async fn peer(
    mut stream: TcpStream,
    route: &Route,
    origin: &crate::config::Origin,
    parsed: &Head,
    head: &str,
    early: &str,
    context: &Context,
) {
    let mut upstream = match crate::forward::Link::connect(origin, &context.tools).await {
        Ok(link) => link,
        Err(failure) => {
            let refusal = Refusal::PeerUnreachable {
                host: route.host.clone(),
                origin: origin.as_str().to_string(),
                detail: match failure {
                    crate::forward::Failure::Timeout => format!(
                        "nothing answered within {} seconds",
                        crate::forward::CONNECT_TIMEOUT.as_secs()
                    ),
                    crate::forward::Failure::Sentence(detail) => detail,
                },
            };
            refuse(&mut stream, &refusal, parsed).await;
            return;
        }
    };
    let Some(authority) = crate::forward::authority(origin) else {
        let refusal = Refusal::PeerUnreachable {
            host: route.host.clone(),
            origin: origin.as_str().to_string(),
            detail: "it is not an origin".to_string(),
        };
        refuse(&mut stream, &refusal, parsed).await;
        return;
    };
    // `/workshop/<id>/x` reaches the peer as `/local/<id>/x`: one spelling for "this machine's session", which is
    // the name a row is not allowed to take (ADR-0073) and the name the peer's own listener accepts.
    let target = format!("/{HOST}/{}{}", route.id, route.rest);
    let rewritten = rewrite(
        head,
        &Target {
            host: authority,
            target: &target,
            origin: Some(origin.as_str()),
        },
        parsed.upgrade,
    );
    if let Err(error) = upstream.write_all(rewritten.as_bytes()).await {
        let refusal = Refusal::PeerUnreachable {
            host: route.host.clone(),
            origin: origin.as_str().to_string(),
            detail: error.to_string(),
        };
        refuse(&mut stream, &refusal, parsed).await;
        return;
    }
    if !early.is_empty() && upstream.write_all(early.as_bytes()).await.is_err() {
        return;
    }
    let (up, down) = tunnel::pump_both(stream, upstream, Tunnel::Forward).await;
    tracing::debug!(
        target: "holosmith::forward",
        host = %route.host,
        session = %route.id,
        path = %route.rest,
        upgrade = parsed.upgrade,
        peer = %origin.as_str(),
        up,
        down,
        "preview forwarded"
    );
}

/// Answer one refusal and log it by its code, so a log line and a test name the same refusal.
async fn refuse(stream: &mut TcpStream, refusal: &Refusal, head: &Head) {
    let _ = respond(stream, refusal.status(), &refusal.sentence()).await;
    tracing::debug!(
        target: "holosmith::preview",
        code = refusal.code(),
        path = %head.target,
        status = refusal.status(),
        "preview refused"
    );
}

/// The one sentence both 400 paths answer, so they cannot drift apart.
const NOT_A_HEAD: &str = "this listener reads one HTTP/1.1 request, and what arrived was not one";

/// The one redirect this listener makes: a page asked for without its trailing slash.
///
/// `308` rather than `301`/`302`, because the method and the body must survive it: `/local/<id>` and
/// `/local/<id>/` are the same resource, and a dev server's form post deserves the same answer a `GET` gets.
async fn redirect_to(stream: &mut TcpStream, location: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 308 Permanent Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\n\
         Connection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await
}

/// One plain-text answer, for every status that is not a proxied exchange.
async fn respond(stream: &mut TcpStream, status: u16, sentence: &str) -> std::io::Result<()> {
    let reason = match status {
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let body = format!("{sentence}\n");
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "http://localhost:8081";
    const ID: &str = "0123456789abcdef";

    fn address() -> SocketAddr {
        SocketAddr::from(([172, 18, 0, 2], 3000))
    }

    /// `rewrite` for the local arm, which is what every test here but the peer one drives.
    fn rewritten_head(head: &str, route: &Route, upgrade: bool) -> String {
        let host = address().to_string();
        rewrite(
            head,
            &Target {
                host: &host,
                target: &route.rest,
                origin: None,
            },
            upgrade,
        )
    }

    #[test]
    fn a_path_is_split_into_host_id_and_rest() {
        let route = Route::strip("/local/0123456789abcdef/index.html").expect("a preview path");
        assert_eq!(route.host, "local");
        assert_eq!(route.id, ID);
        assert_eq!(route.rest, "/index.html");
    }

    #[test]
    fn the_query_is_kept_and_the_prefix_is_stripped() {
        let route = Route::strip("/local/0123456789abcdef/api/x?y=1&z=2").expect("a preview path");
        assert_eq!(route.rest, "/api/x?y=1&z=2");
    }

    #[test]
    fn a_path_with_no_rest_is_the_upstreams_root() {
        for path in ["/local/0123456789abcdef", "/local/0123456789abcdef/"] {
            assert_eq!(
                Route::strip(path).expect("a preview path").rest,
                "/",
                "{path}"
            );
        }
        assert_eq!(
            Route::strip("/local/0123456789abcdef?x=1")
                .expect("a preview path")
                .rest,
            "/?x=1"
        );
    }

    #[test]
    fn the_host_is_a_value_and_a_name_the_registry_does_not_hold_is_refused_by_the_decision() {
        // HS-069: `strip` splits, and `plan` decides. A name nobody registered is still the 404 it was, with a
        // sentence that names the name — the answer a browser opening an old link gets on a hub.
        let route = Route::strip("/studio/0123456789abcdef/").expect("a preview path");
        assert_eq!(route.host, "studio");
        let refusal = plan(&route.host, crate::config::Role::Both, None).expect_err("no such host");
        assert_eq!(refusal.code(), "preview_unknown_host");
        assert_eq!(refusal.status(), 404);
        assert!(refusal.sentence().contains("studio"), "{refusal}");
    }

    #[test]
    fn the_four_arms_of_the_decision() {
        // 1. `local` on every role, and without a row.
        for role in [
            crate::config::Role::Hub,
            crate::config::Role::Server,
            crate::config::Role::Both,
        ] {
            assert_eq!(plan(HOST, role, None), Ok(Plan::Local), "{role:?}");
        }
        // 2. A name no row holds is the same answer on every role ...
        let refusal = plan("workshop", crate::config::Role::Server, None).expect_err("a server");
        assert_eq!(refusal.code(), "preview_unknown_host");
        assert_eq!(refusal.status(), 404);
        // 4. A row with no preview origin: the peer said nothing about where its previews answer.
        let row = crate::hosts::Host {
            name: "workshop".to_string(),
            url: crate::config::Origin::parse("https://workshop.example:443").expect("an origin"),
            preview: None,
            reach: crate::hosts::Reach::Reachable,
            version: Some("0.1.0".to_string()),
            last_error: None,
            last_checked_at: 1,
        };
        let refusal =
            plan("workshop", crate::config::Role::Hub, Some(&row)).expect_err("no preview origin");
        assert_eq!(refusal.code(), "preview_no_peer_preview");
        assert_eq!(refusal.status(), 502);
        // 3. A row that does know, and a hub that forwards.
        let row = crate::hosts::Host {
            preview: Some(
                crate::config::Origin::parse("https://workshop.example:8444").expect("an origin"),
            ),
            ..row
        };
        assert_eq!(
            plan("workshop", crate::config::Role::Hub, Some(&row)),
            Ok(Plan::Peer(
                crate::config::Origin::parse("https://workshop.example:8444").expect("an origin")
            ))
        );
        // A `both` machine forwards too: `Roles` says "any machine can be the hub".
        assert_eq!(
            plan("workshop", crate::config::Role::Both, Some(&row)),
            Ok(Plan::Peer(
                crate::config::Origin::parse("https://workshop.example:8444").expect("an origin")
            ))
        );
        // 3. ... and a name a `server` **does** hold is the machine that forwards nothing, not an unknown name.
        let refusal =
            plan("workshop", crate::config::Role::Server, Some(&row)).expect_err("a server");
        assert_eq!(refusal.code(), "preview_not_forwarded");
        assert_eq!(refusal.status(), 403);
    }

    #[test]
    fn a_segment_that_is_not_a_session_id_is_the_local_tables_answer() {
        // The id's alphabet is the row's, not the path's (HS-069): a peer's session is none of this machine's
        // business, and the local arm answers "no session is called that" from its own table.
        for path in [
            "/local/nope/",
            "/local/0123456789ABCDEF/",
            "/local/0123456789abcde/",
            "/local/0123456789abcdef0/",
            "/local/../",
        ] {
            let route = Route::strip(path).expect(path);
            assert_eq!(route.id, path.split('/').nth(2).expect("an id"), "{path}");
            assert_eq!(
                plan(&route.host, crate::config::Role::Both, None),
                Ok(Plan::Local)
            );
        }
    }

    #[test]
    fn a_forwarded_request_names_the_peer_and_claims_the_peers_origin() {
        // What a hub sends the machine that owns the session: the path spelled the way that machine spells its own
        // (`local`), its preview origin in `Host`, and the same origin in `Origin` — which is the header its listener
        // compares against its own config, and the reason `hosts.preview` exists (ADR-0074 D5).
        let route =
            Route::strip("/workshop/0123456789abcdef/index.html?x=1").expect("a preview path");
        let target = format!("/{HOST}/{}{}", route.id, route.rest);
        let rewritten = rewrite(
            "GET /workshop/0123456789abcdef/index.html?x=1 HTTP/1.1\r\nHost: hub.example\r\n\
             Origin: https://hub.example:8444\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
            &Target {
                host: "workshop.example:8444",
                target: &target,
                origin: Some("https://workshop.example:8444"),
            },
            true,
        );
        assert_eq!(
            rewritten,
            "GET /local/0123456789abcdef/index.html?x=1 HTTP/1.1\r\nHost: workshop.example:8444\r\n\
             Origin: https://workshop.example:8444\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n"
        );
        // And an origin the client did not send is added rather than left missing, or the peer refuses the upgrade.
        let rewritten = rewrite(
            "GET /local/0123456789abcdef/ws HTTP/1.1\r\nHost: h\r\nUpgrade: websocket\r\n\r\n",
            &Target {
                host: "workshop.example:8444",
                target: "/local/0123456789abcdef/ws",
                origin: Some("https://workshop.example:8444"),
            },
            true,
        );
        assert!(
            rewritten.contains("Origin: https://workshop.example:8444\r\n"),
            "{rewritten}"
        );
    }

    #[test]
    fn a_path_with_no_host_and_id_is_its_own_refusal() {
        for path in ["/", "/local", "/local/"] {
            let refusal = Route::strip(path).expect_err(path);
            assert_eq!(refusal.code(), "preview_not_a_path", "{path}");
        }
    }

    #[test]
    fn a_control_api_path_is_a_host_this_listener_does_not_serve() {
        // `/api/health` is `/<host>/<id>` to a listener that serves only `/<host>/<id>/…`, so the **host** segment
        // is what refuses it: `api` is a name, and no row has it. That is the same 404 a browser gets, and the
        // sentence names a preview path. The gate's `case_preview` asserts the answer rather than the code: no
        // envelope, and a path it can point at.
        for path in ["/api/health", "/api/sessions"] {
            let route = Route::strip(path).expect(path);
            assert_eq!(route.host, "api", "{path}");
            let refusal = plan(&route.host, crate::config::Role::Both, None).expect_err(path);
            assert_eq!(refusal.code(), "preview_unknown_host", "{path}");
            assert_eq!(refusal.status(), 404, "{path}");
            assert!(refusal.sentence().contains("/local/"), "{path}: {refusal}");
        }
    }

    #[test]
    fn only_the_configured_origin_is_accepted_for_an_upgrade() {
        let origin = crate::config::Origin::parse(ORIGIN).expect("a test origin is valid");
        assert!(from_preview_origin(&origin, Some(ORIGIN)));
        assert!(from_preview_origin(
            &origin,
            Some(&ORIGIN.to_ascii_uppercase())
        ));
        assert!(from_preview_origin(
            &origin,
            Some(" http://localhost:8081 ")
        ));
        assert!(!from_preview_origin(&origin, None));
        assert!(!from_preview_origin(&origin, Some("http://evil.example")));
        assert!(!from_preview_origin(
            &origin,
            Some("http://localhost:8081.evil.example")
        ));
    }

    #[test]
    fn a_head_is_its_request_line_its_origin_and_whether_it_upgrades() {
        let head = parse_head(
            "GET /local/0123456789abcdef/ HTTP/1.1\r\nHost: a\r\nOrigin: http://localhost:8081\r\n\
             Upgrade: websocket\r\n\r\n",
        )
        .expect("a head");
        assert_eq!(head.method, "GET");
        assert_eq!(head.target, "/local/0123456789abcdef/");
        assert_eq!(head.origin.as_deref(), Some(ORIGIN));
        assert!(head.upgrade);
    }

    #[test]
    fn two_origin_headers_resolve_to_neither() {
        let head = parse_head("GET / HTTP/1.1\r\nOrigin: a\r\nOrigin: b\r\n\r\n").expect("a head");
        assert_eq!(head.origin, None);
        let head = parse_head("GET / HTTP/1.1\r\nOrigin: a\r\nOrigin: b\r\nOrigin: c\r\n\r\n")
            .expect("a head");
        assert_eq!(head.origin, None);
    }

    #[test]
    fn a_request_line_that_is_not_one_is_refused() {
        assert!(parse_head("").is_none());
        assert!(parse_head("hello\r\n\r\n").is_none());
        assert!(parse_head("GET\r\n\r\n").is_none());
        assert!(
            parse_head("GET http://x/ HTTP/1.1\r\n\r\n").is_none(),
            "an absolute-form target is not a preview path"
        );
    }

    #[test]
    fn the_rewritten_head_strips_the_prefix_and_names_the_container() {
        let route = Route::strip("/local/0123456789abcdef/index.html?x=1").expect("a preview path");
        let head = "GET /local/0123456789abcdef/index.html?x=1 HTTP/1.1\r\nHost: localhost:8081\r\n\
                    Accept: */*\r\n\r\n";
        let rewritten = rewritten_head(head, &route, false);
        assert_eq!(
            rewritten,
            "GET /index.html?x=1 HTTP/1.1\r\nHost: 172.18.0.2:3000\r\nAccept: */*\r\n\
             Connection: close\r\n\r\n"
        );
    }

    #[test]
    fn an_upgrade_keeps_its_connection_header() {
        let route = Route::strip("/local/0123456789abcdef/ws").expect("a preview path");
        let head = "GET /local/0123456789abcdef/ws HTTP/1.1\r\nHost: h\r\nConnection: Upgrade\r\n\
                    Upgrade: websocket\r\nSec-WebSocket-Key: k\r\n\r\n";
        let rewritten = rewritten_head(head, &route, true);
        assert!(rewritten.contains("Connection: Upgrade\r\n"), "{rewritten}");
        assert!(!rewritten.contains("Connection: close"), "{rewritten}");
        assert!(rewritten.contains("Upgrade: websocket\r\n"), "{rewritten}");
    }

    #[test]
    fn a_host_that_was_not_sent_is_added_and_a_duplicate_is_replaced() {
        let route = Route::strip("/local/0123456789abcdef/").expect("a preview path");
        let rewritten = rewritten_head("GET / HTTP/1.1\r\n\r\n", &route, false);
        assert!(
            rewritten.contains("Host: 172.18.0.2:3000\r\n"),
            "{rewritten}"
        );
        let rewritten = rewritten_head(
            "GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n",
            &route,
            false,
        );
        assert_eq!(rewritten.matches("Host: ").count(), 2, "{rewritten}");
        assert!(!rewritten.contains("Host: a"), "{rewritten}");
    }

    #[test]
    fn a_body_that_arrived_with_the_head_is_kept_apart_from_it() {
        let (head, rest) = split_head("POST /x HTTP/1.1\r\nHost: a\r\n\r\nhello");
        assert_eq!(head, "POST /x HTTP/1.1\r\nHost: a\r\n\r\n");
        assert_eq!(rest, "hello");
        let (head, rest) = split_head("POST /x HTTP/1.1\nHost: a\n\nhello");
        assert_eq!(head, "POST /x HTTP/1.1\nHost: a\n\n");
        assert_eq!(rest, "hello");
        let (head, rest) = split_head("GET /x HTTP/1.1\r\n\r\n");
        assert_eq!(head, "GET /x HTTP/1.1\r\n\r\n");
        assert_eq!(rest, "");
    }

    #[test]
    fn only_the_slashless_root_is_redirected_and_the_query_survives() {
        let route = Route::strip("/local/0123456789abcdef").expect("a preview path");
        assert!(route.slashless_root);
        assert_eq!(route.slashed_path(), format!("/local/{ID}/"));
        let route = Route::strip("/local/0123456789abcdef?x=1").expect("a preview path");
        assert!(route.slashless_root);
        assert_eq!(route.slashed_path(), format!("/local/{ID}/?x=1"));
        // A path that already ends in the slash is not redirected, and neither is one deeper than the root: only
        // the session's own root has a base URL a browser would resolve its relative URLs against.
        for path in [
            "/local/0123456789abcdef/",
            "/local/0123456789abcdef/a/b",
            "/local/0123456789abcdef/a/",
        ] {
            assert!(
                !Route::strip(path).expect(path).slashless_root,
                "{path} must not be redirected"
            );
        }
    }

    #[test]
    fn the_path_the_screen_links_to_is_the_one_this_listener_serves() {
        assert_eq!(Route::path(ID), format!("/local/{ID}/"));
        assert_eq!(
            Route::strip(&Route::path(ID)).expect("its own path").rest,
            "/"
        );
    }

    #[test]
    fn every_refusal_has_a_status_a_code_and_a_sentence() {
        let refusals = [
            Refusal::NotAPath {
                path: "/".to_string(),
            },
            Refusal::UnknownHost("studio".to_string()),
            Refusal::UnknownSession(ID.to_string()),
            Refusal::NoContainer {
                id: ID.to_string(),
                detail: "gone".to_string(),
            },
            Refusal::Unreachable {
                id: ID.to_string(),
                address: address(),
                detail: "refused".to_string(),
            },
            Refusal::Unreadable {
                detail: "locked".to_string(),
            },
            Refusal::ForeignOrigin {
                expected: ORIGIN.to_string(),
                sent: None,
            },
            Refusal::NotForwarded {
                host: "workshop".to_string(),
            },
            Refusal::NoPreview {
                host: "workshop".to_string(),
            },
            Refusal::PeerUnreachable {
                host: "workshop".to_string(),
                origin: "https://workshop.example:8444".to_string(),
                detail: "refused".to_string(),
            },
        ];
        for refusal in refusals {
            assert!(!refusal.sentence().is_empty(), "{refusal:?}");
            assert!(!refusal.code().is_empty(), "{refusal:?}");
            assert!(
                matches!(refusal.status(), 400 | 403 | 404 | 500 | 502),
                "{refusal:?} answered {}",
                refusal.status()
            );
        }
    }
}
