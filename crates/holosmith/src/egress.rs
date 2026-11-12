//! The egress proxy: the only route out of a session container (spec item 2).
//!
//! Environment proxy variables are advisory, so item 2 moves enforcement to the network: a session's
//! network is `--internal`, the proxy container is attached to the machine's `egress` network *and* to that
//! session network, and the container's `HTTPS_PROXY` names the proxy ([`crate::container::EgressProxy`]).
//! Everything a container can reach therefore arrives here, and this module is the decision about it:
//!
//! * `CONNECT` only, and **port 443 only** — a tunnel is the only thing this proxy does, because anything
//!   else it could do is a plaintext request it would have to understand ([`Refusal::NotTunnel`]).
//! * **Only allow-listed domains** ([`Rules::allow`]), an *exact* match. `config` already refuses a
//!   wildcard in `[egress] allow` because that is a claim about every subdomain; a suffix match here would
//!   be the same claim under another name (ADR-0045).
//! * **No destination that resolves into the machine.** Loopback, link-local, the RFC 1918 ranges,
//!   100.64.0.0/10 and the IPv6 equivalents are refused *whatever the allow-list says*, because a name an
//!   operator allow-listed can still resolve somewhere it must never reach — and item 2's own measurement
//!   (ADR-0044) is that a container's network gateway answers, so `localhost` is not an academic case.
//! * **Every** answer a name has is checked: one private address refuses the whole name, so a dual-answer
//!   host cannot be reached through the address the check did not look at.
//!
//! Two things are deliberately *not* here. Bytes are counted and the host is logged per request
//! ([`Record`]), but the content is not inspected: HTTPS is opaque to a `CONNECT` tunnel, which is why item
//! 2 lists "every allow-listed host is an exfiltration channel" as an accepted risk. And a provider host is
//! not on the list at all — the key proxy is what makes a model call work, so that path never
//! passes through this container.
//!
//! The split below is the reason this module is testable without a daemon, a network or a browser: [`Rules`]
//! decides from values ([`Request::parse`], [`Rules::target`], [`address_problem`]), and the tokio half only
//! moves bytes through a decision it was given — the same shape `crate::exit::classify` and
//! `crate::selftest::decide` have.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

/// The port the proxy listens on, and the port `HTTPS_PROXY` names (spec item 2).
pub const PORT: u16 = 3128;

/// The only port a tunnel may target: `CONNECT` to anything else is refused (spec item 2).
pub const TLS_PORT: u16 = 443;

/// Where the proxy listens unless `--listen` says otherwise.
///
/// The wildcard address, because the container's callers arrive on a Docker network interface the build
/// does not know; a host-side run and every test pass an explicit `127.0.0.1:<port>`.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:3128";

/// The longest request head this proxy reads. A `CONNECT` request is two lines and a header or two; the bound
/// is what keeps a client that never sends a blank line from holding a task.
pub const MAX_HEAD: usize = 8 * 1024;

/// How long a client may take to send its request head.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest pause between two failed `accept`s: a listener that is gone must not fill the log at full speed,
/// and a transient failure must not cost more than a moment.
pub const ACCEPT_BACKOFF: Duration = Duration::from_secs(5);

/// How long a name may take to resolve, and a connection to be made. Both are `warn`-worthy failures rather
/// than a hung request, and neither is a policy about the machine.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The username a request is attributed to when it sent no `Proxy-Authorization`.
pub const UNATTRIBUTED: &str = "-";

/// One request, as the proxy read it: the method, the target and whatever the client claimed about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub target: String,
    /// The session id the client sent as the proxy username, when it sent a usable one.
    pub session: Option<String>,
}

impl Request {
    /// Parse a request head (everything up to and including the blank line).
    ///
    /// Deliberately tolerant of what it does not need and strict about what it does: the first line must have
    /// three space-separated fields, and `Proxy-Authorization` is read case-insensitively by header name
    /// because nothing normalises a header for us. A head that does not parse is a 400 and a closed
    /// connection — this proxy has no keep-alive, so there is nothing to resynchronise.
    pub fn parse(head: &str) -> Option<Self> {
        let mut lines = head.split("\r\n").flat_map(|line| line.split('\n'));
        let request_line = lines.next()?;
        let mut fields = request_line.split(' ').filter(|field| !field.is_empty());
        let method = fields.next()?.to_string();
        let target = fields.next()?.to_string();
        let version = fields.next()?;
        if !version.starts_with("HTTP/") || fields.next().is_some() {
            return None;
        }
        let mut session = None;
        for line in lines {
            if line.is_empty() {
                break;
            }
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.trim().eq_ignore_ascii_case("proxy-authorization") {
                session = username(value.trim());
            }
        }
        Some(Self {
            method,
            target,
            session,
        })
    }
}

/// The session id in a `Proxy-Authorization` value: `Basic <base64 user:password>`.
///
/// Only the username is read, and only when it is a shape a session id has ([`crate::tool::valid_id`]): an
/// unvalidated string reaches a log line, and a log line with an escape sequence in it is a log line that
/// lies about what happened. A password is not a credential here — nothing checks it — so it is not read.
fn username(value: &str) -> Option<String> {
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = crate::tool::decode_base64(encoded.trim())?;
    let text = String::from_utf8(decoded).ok()?;
    let user = text.split(':').next()?;
    crate::tool::valid_id(user).then(|| user.to_string())
}

/// Where a request wants to go, once the rules have agreed it may.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The name, lowercased and without a trailing root dot: how it is matched and how it is logged.
    pub host: String,
    pub port: u16,
}

/// Why a request was not tunnelled. Each answer has a status, a word for the log and a sentence for the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Anything that is not `CONNECT`: this proxy only tunnels.
    NotTunnel { method: String },
    /// `CONNECT` to a port that is not 443 (item 2's rule).
    NotTls { host: String, port: u16 },
    /// A target that is not `host:port`.
    Malformed { target: String },
    /// Not on the allow-list. Carries the name it asked for.
    NotListed { host: String },
    /// On the allow-list, but one of its addresses is inside this machine (item 2's "regardless of the
    /// allow-list"). `range` names the range in the operator's words.
    Inside {
        host: String,
        address: IpAddr,
        range: &'static str,
    },
    /// The name did not resolve, or the connection to it was not made.
    Unreachable { host: String, detail: String },
}

impl Refusal {
    /// The status a caller sees: 405 for the wrong method (the proxy says what it does), 403 for the policy,
    /// 400 for a head this proxy cannot read, 502 for a target it could not reach.
    pub fn status(&self) -> u16 {
        match self {
            Self::NotTunnel { .. } => 405,
            Self::Malformed { .. } => 400,
            Self::Unreachable { .. } => 502,
            Self::NotTls { .. } | Self::NotListed { .. } | Self::Inside { .. } => 403,
        }
    }

    /// The word the log line carries: one spelling per reason, and it is the number the operator greps.
    pub fn word(&self) -> &'static str {
        match self {
            Self::NotTunnel { .. } => "not-connect",
            Self::NotTls { .. } => "not-443",
            Self::Malformed { .. } => "malformed",
            Self::NotListed { .. } => "not-listed",
            Self::Inside { .. } => "inside",
            Self::Unreachable { .. } => "unreachable",
        }
    }

    /// The host the log line names, when the request got far enough to have one.
    pub fn host(&self) -> &str {
        match self {
            Self::NotTunnel { .. } | Self::Malformed { .. } => UNATTRIBUTED,
            Self::NotTls { host, .. }
            | Self::NotListed { host }
            | Self::Inside { host, .. }
            | Self::Unreachable { host, .. } => host,
        }
    }

    /// The sentence in the body. Written for the caller — a `curl` in a container, whose operator reads it in
    /// a transcript — so it says what the proxy will not do and what would change that, not a policy number.
    pub fn sentence(&self) -> String {
        match self {
            Self::NotTunnel { method } => {
                format!("this proxy tunnels HTTPS only: send CONNECT, not {method}")
            }
            Self::NotTls { host, port } => {
                format!("{host}:{port} is not port {TLS_PORT}: this proxy tunnels HTTPS only")
            }
            Self::Malformed { target } => {
                format!("{target:?} is not a host and port: send CONNECT host:port")
            }
            Self::NotListed { host } => format!(
                "{host} is not on this machine's egress allow-list; ask the operator to add it"
            ),
            Self::Inside { address, range, .. } => format!(
                "{address} is {range}: this proxy never tunnels into the machine, whatever the \
                 allow-list says"
            ),
            Self::Unreachable { host, detail } => format!("{host} could not be reached: {detail}"),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sentence())
    }
}

/// The proxy's policy: the allow-list and the port rule, with no I/O of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    allow: Vec<String>,
}

impl Rules {
    /// The rules an `[egress] allow` list makes. Entries are lowercased and a trailing root dot dropped, so a
    /// `--allow Crates.IO` typed on a command line is the same rule as the `crates.io` the config pass would
    /// have demanded in the file (it refuses an uppercase entry; the flag is normalised instead of refused,
    /// because a container has no file to correct).
    pub fn new<I, S>(allow: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            allow: allow
                .into_iter()
                .map(|entry| canonical(entry.as_ref()))
                .filter(|entry| !entry.is_empty())
                .collect(),
        }
    }

    /// The rules as they were written, normalised: what a listener prints at startup and what the log line's
    /// neighbouring `allow=<n>` counts.
    pub fn allow(&self) -> &[String] {
        &self.allow
    }

    /// Whether `host` may be tunnelled to, on the exact spelling (ADR-0045).
    pub fn permits(&self, host: &str) -> bool {
        let host = canonical(host);
        self.allow.contains(&host)
    }

    /// The target this request asks for, or the reason it is refused before any name is resolved.
    ///
    /// Order matters: the method first (it decides the status class), then the shape of the target, then the
    /// port, then the list. Resolution is the caller's, because it is the one thing here that is not a
    /// decision about values.
    pub fn target(&self, request: &Request) -> Result<Target, Refusal> {
        if request.method != "CONNECT" {
            return Err(Refusal::NotTunnel {
                method: request.method.clone(),
            });
        }
        let Some((host, port)) = split_target(&request.target) else {
            return Err(Refusal::Malformed {
                target: request.target.clone(),
            });
        };
        if port != TLS_PORT {
            return Err(Refusal::NotTls { host, port });
        }
        if !self.permits(&host) {
            return Err(Refusal::NotListed { host });
        }
        Ok(Target { host, port })
    }
}

/// `host:port`, with an IPv6 literal in brackets, as `CONNECT` spells both.
fn split_target(target: &str) -> Option<(String, u16)> {
    if target.contains('/') || target.contains(' ') {
        return None;
    }
    let (host, port) = if let Some(rest) = target.strip_prefix('[') {
        let (inside, after) = rest.split_once(']')?;
        inside.parse::<Ipv6Addr>().ok()?;
        (inside, after.strip_prefix(':')?)
    } else {
        let (host, port) = target.rsplit_once(':')?;
        if host.is_empty() || host.contains(':') {
            return None;
        }
        (host, port)
    };
    let port = port.parse::<u16>().ok()?;
    let host = canonical(host);
    (!host.is_empty()).then_some((host, port))
}

/// A name as the rules and the log line spell it: lowercase, no trailing root dot.
fn canonical(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Why `address` may not be tunnelled to, in the operator's words — `None` when it may.
///
/// The four ranges item 2 names, plus the two spellings a host has that mean the same thing: the unspecified
/// address (which is "this machine, whichever interface" to `connect`) and IPv6's own private range. A
/// `::ffff:127.0.0.1` is a loopback address whatever family it is written in, so a mapped address is judged
/// as the IPv4 address it carries — otherwise the check is one spelling deep.
pub fn address_problem(address: IpAddr) -> Option<&'static str> {
    match address {
        IpAddr::V4(v4) => ipv4_problem(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return ipv4_problem(mapped);
            }
            if v6.is_loopback() {
                Some("loopback (::1)")
            } else if v6.is_unspecified() {
                Some("the unspecified address")
            } else if is_v6_link_local(v6) {
                Some("link-local (fe80::/10)")
            } else if is_v6_unique_local(v6) {
                Some("a unique local address (fc00::/7)")
            } else if is_v6_site_local(v6) {
                Some("a site-local address (fec0::/10)")
            } else if let Some(embedded) = embedded_v4(v6) {
                // 6to4, NAT64 and IPv4-compatible addresses all *carry* a v4 destination, and a route for one of
                // them reaches exactly that v4 address: `64:ff9b::7f00:1` is `127.0.0.1` on a NAT64 network. One
                // recursion, so the check is as deep as the addressing is.
                ipv4_problem(embedded)
            } else {
                None
            }
        }
    }
}

fn ipv4_problem(address: Ipv4Addr) -> Option<&'static str> {
    let [a, b, ..] = address.octets();
    if address.is_loopback() {
        Some("loopback (127.0.0.0/8)")
    } else if address.is_unspecified() {
        Some("the unspecified address")
    } else if a == 0 {
        // The rest of 0.0.0.0/8 is "this network" (RFC 1122): a `connect` to 0.0.0.1 is a connect to this machine.
        Some("this network (0.0.0.0/8)")
    } else if address.is_link_local() {
        Some("link-local (169.254.0.0/16)")
    } else if a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168) {
        Some("a private range (RFC 1918)")
    } else if a == 100 && (64..=127).contains(&b) {
        Some("the shared address space (100.64.0.0/10)")
    } else {
        None
    }
}

fn is_v6_link_local(address: Ipv6Addr) -> bool {
    (address.segments()[0] & 0xffc0) == 0xfe80
}

fn is_v6_unique_local(address: Ipv6Addr) -> bool {
    (address.segments()[0] & 0xfe00) == 0xfc00
}

/// `fec0::/10`, the deprecated site-local range: the same "inside this network" claim as `fc00::/7`.
fn is_v6_site_local(address: Ipv6Addr) -> bool {
    (address.segments()[0] & 0xffc0) == 0xfec0
}

/// The IPv4 address an IPv6 address carries, for the three prefixes that carry one: 6to4 (`2002::/16`), NAT64
/// (`64:ff9b::/96`) and IPv4-compatible (`::a.b.c.d`, which `to_ipv4` also answers for `::` and `::1` — both
/// checked before this runs).
fn embedded_v4(address: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = address.segments();
    if segments[0] == 0x2002 {
        return Some(Ipv4Addr::new(
            (segments[1] >> 8) as u8,
            segments[1] as u8,
            (segments[2] >> 8) as u8,
            segments[2] as u8,
        ));
    }
    if segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2..6] == [0, 0, 0, 0] {
        return Some(Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            segments[6] as u8,
            (segments[7] >> 8) as u8,
            segments[7] as u8,
        ));
    }
    // IPv4-compatible: six zero segments, then the address. `to_ipv4` answers `None` for anything else.
    if segments[..6] == [0, 0, 0, 0, 0, 0] {
        return address.to_ipv4();
    }
    None
}

/// One answered request, which is what the log line renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The session id from the proxy username, or [`UNATTRIBUTED`].
    pub session: String,
    pub host: String,
    /// The [`Refusal::word`] for a refusal and `"allowed"` for a tunnel.
    pub status: &'static str,
    /// Bytes the proxy wrote upstream (what the container sent).
    pub up: u64,
    /// Bytes it wrote back (what the container received).
    pub down: u64,
}

impl Record {
    fn refused(session: &str, refusal: &Refusal) -> Self {
        Self {
            session: session.to_string(),
            host: refusal.host().to_string(),
            status: refusal.word(),
            up: 0,
            down: 0,
        }
    }

    /// The one line an operator (and `scripts/verify.sh`) reads: the session, the host, the decision and the
    /// bytes in either direction. Emitted at `info` because it is a per-request fact, not startup noise.
    pub fn log(&self) {
        tracing::info!(
            target: "holosmith::egress",
            session = %self.session,
            host = %self.host,
            status = %self.status,
            bytes_up = self.up,
            bytes_down = self.down,
            "the egress proxy answered a request"
        );
    }
}

/// Serve forever, one task per connection: nothing in this process stops the loop, and the signals that do are
/// the caller's (see [`crate::commands::proxy`]).
pub async fn serve(listener: TcpListener, rules: Arc<Rules>) {
    let mut failures: u32 = 0;
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                failures = 0;
                let rules = Arc::clone(&rules);
                tokio::spawn(async move {
                    handle(stream, &rules).await;
                    tracing::debug!(
                        target: "holosmith::egress",
                        peer = %peer,
                        "the connection is closed"
                    );
                });
            }
            Err(error) => {
                // An accept that fails is a listener this process can no longer serve on, and the two failure
                // modes want different things: a transient one (a full backlog, a connection that died between
                // `accept` and the return) should be retried at once, and a permanent one (a listener that is
                // gone) should not spin a core filling the log. So the pause **escalates** with consecutive
                // failures and resets on a success, and the process's own supervisor (systemd, or the
                // container's restart policy) is what replaces a listener that never comes back.
                failures = failures.saturating_add(1);
                let pause = ACCEPT_BACKOFF.min(Duration::from_millis(100) * failures.min(60));
                tracing::warn!(
                    target: "holosmith::egress",
                    %error,
                    failures,
                    pause_ms = pause.as_millis(),
                    "the egress proxy could not accept a connection"
                );
                tokio::time::sleep(pause).await;
            }
        }
    }
}

/// One connection: read a head, decide, answer, and tunnel when the answer is yes.
///
/// The refusal is written to the caller and logged; nothing is retried, because a proxy that retried a refusal
/// would be a proxy whose log line does not mean what it says.
pub async fn handle(mut stream: TcpStream, rules: &Rules) {
    let head = match crate::tunnel::read_head(&mut stream, MAX_HEAD, HEAD_TIMEOUT).await {
        Some(head) => head,
        None => {
            let _ = respond(&mut stream, 400, NOT_A_HEAD).await;
            Record::unreadable().log();
            return;
        }
    };
    let Some(request) = Request::parse(&head) else {
        let _ = respond(&mut stream, 400, NOT_A_HEAD).await;
        Record::unreadable().log();
        return;
    };
    let session = request
        .session
        .clone()
        .unwrap_or_else(|| UNATTRIBUTED.to_string());
    let target = match rules.target(&request) {
        Ok(target) => target,
        Err(refusal) => {
            let _ = respond(&mut stream, refusal.status(), &refusal.sentence()).await;
            Record::refused(&session, &refusal).log();
            return;
        }
    };
    let addresses = match resolve(&target.host).await {
        Ok(addresses) if !addresses.is_empty() => addresses,
        Ok(_) => {
            let refusal = Refusal::Unreachable {
                host: target.host.clone(),
                detail: "it resolved to no address".to_string(),
            };
            let _ = respond(&mut stream, refusal.status(), &refusal.sentence()).await;
            Record::refused(&session, &refusal).log();
            return;
        }
        Err(detail) => {
            let refusal = Refusal::Unreachable {
                host: target.host.clone(),
                detail,
            };
            let _ = respond(&mut stream, refusal.status(), &refusal.sentence()).await;
            Record::refused(&session, &refusal).log();
            return;
        }
    };
    // Every answer, not the first usable one: a name with a public and a loopback address must not be
    // reachable through the address the check did not look at.
    for address in &addresses {
        if let Some(range) = address_problem(address.ip()) {
            let refusal = Refusal::Inside {
                host: target.host.clone(),
                address: address.ip(),
                range,
            };
            let _ = respond(&mut stream, refusal.status(), &refusal.sentence()).await;
            Record::refused(&session, &refusal).log();
            return;
        }
    }
    Record::tunnel(stream, &target.host, session, &addresses)
        .await
        .log();
}

/// `NOT_A_HEAD`: the one sentence both 400 paths answer, so they cannot drift apart.
const NOT_A_HEAD: &str =
    "the request head is not a CONNECT request: send CONNECT host:port HTTP/1.1";

/// Tunnel one connection to the first of `addresses` that answers, and answer the record of what happened.
///
/// Separate from [`handle`] for one reason: the targets here have already passed the policy, so a test can drive
/// this against a listener on a loopback port — which the policy refuses — and still exercise the `200` that
/// must carry **no body** and the byte counting that must survive an error. `src/egress.rs`'s own test does
/// exactly that, because the story's demo is the only other thing that would have caught it (`curl` answered
/// `wrong version number` when the `200` carried a `Content-Length`).
///
/// **The counts are `AtomicU64`s the copy loop bumps as bytes move**, not the return value of a `copy`: a copy
/// that fails reports no count at all, so a tunnel that carried a megabyte and then died would log
/// `bytes_up=0` — precisely the request an operator wants the numbers for.
impl Record {
    /// The record for a head that could not be read: no host, no session, no bytes, and a word of its own.
    fn unreadable() -> Self {
        Self {
            session: UNATTRIBUTED.to_string(),
            host: UNATTRIBUTED.to_string(),
            status: "not-http",
            up: 0,
            down: 0,
        }
    }

    /// Tunnel `stream` to `addresses` and answer the record: `allowed` with what moved, or `unreachable` when
    /// nothing answered or the `200` could not be written.
    pub(crate) async fn tunnel(
        mut stream: TcpStream,
        host: &str,
        session: String,
        addresses: &[SocketAddr],
    ) -> Self {
        let Some(upstream) = connect(addresses).await else {
            let _ = respond(
                &mut stream,
                502,
                &format!("{host} could not be reached: nothing answered on port {TLS_PORT}"),
            )
            .await;
            return Self {
                session,
                host: host.to_string(),
                status: "unreachable",
                up: 0,
                down: 0,
            };
        };
        if establish(&mut stream).await.is_err() {
            // The client left before the tunnel existed: nothing moved, and the connection is not a request the
            // proxy can answer.
            return Self {
                session,
                host: host.to_string(),
                status: "unanswered",
                up: 0,
                down: 0,
            };
        }
        let (up, down) =
            crate::tunnel::pump_both(stream, upstream, crate::tunnel::Tunnel::Egress).await;
        Self {
            session,
            host: host.to_string(),
            status: "allowed",
            up,
            down,
        }
    }
}

/// The name's addresses, resolved off the async worker because `getaddrinfo` blocks.
///
/// A block, not a rewrite: the proxy has no resolver of its own, the container's `/etc/resolv.conf` (Docker's
/// embedded DNS) is the one that knows the names this machine has, and `spawn_blocking` is what keeps the
/// proxy accepting other connections while one name is looked up.
async fn resolve(host: &str) -> Result<Vec<SocketAddr>, String> {
    let host = host.to_string();
    let looked_up = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            (host.as_str(), TLS_PORT)
                .to_socket_addrs()
                .map(|addresses| addresses.collect::<Vec<_>>())
        }),
    )
    .await;
    match looked_up {
        Ok(Ok(Ok(addresses))) => Ok(addresses),
        Ok(Ok(Err(error))) => Err(error.to_string()),
        Ok(Err(error)) => Err(format!("the lookup did not finish ({error})")),
        Err(_) => Err(format!(
            "the name did not resolve within {} seconds",
            CONNECT_TIMEOUT.as_secs()
        )),
    }
}

/// Connect to the first of `addresses` that answers, bounded by [`CONNECT_TIMEOUT`].
///
/// Every address here has already passed [`address_problem`], so this is only about which one is up: a name
/// with two public addresses should not fail because the first is down.
async fn connect(addresses: &[SocketAddr]) -> Option<TcpStream> {
    for address in addresses {
        if let Ok(stream) = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await
            && let Ok(stream) = stream
        {
            return Some(stream);
        }
    }
    None
}

/// The one answer a `CONNECT` that is going to be tunnelled gets, and it is **not** an HTTP message body.
///
/// A `200` to `CONNECT` switches the connection to a tunnel: everything after the blank line is the client's
/// TLS handshake, so a `Content-Length`, a `Content-Type` or a body here would be read as the server's first
/// TLS record — `curl` answers `error:0A00010B:SSL routines::wrong version number`, which is exactly what the
/// story's demo produced before this was split out of [`respond`]. There is no `Proxy-Agent` either, for the
/// same reason: the fewer bytes between the answer and the tunnel, the fewer ways to get it wrong.
async fn establish(stream: &mut TcpStream) -> std::io::Result<()> {
    stream
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    stream.flush().await
}

/// One HTTP answer with a sentence in it, for every status that is *not* a tunnel.
async fn respond(stream: &mut TcpStream, status: u16, sentence: &str) -> std::io::Result<()> {
    let reason = match status {
        400 => "Bad Request",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let body = format!("{sentence}\n");
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\
         Proxy-Agent: holosmith\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    fn connect_request(target: &str) -> Request {
        Request {
            method: "CONNECT".to_string(),
            target: target.to_string(),
            session: None,
        }
    }

    fn rules(allow: &[&str]) -> Rules {
        Rules::new(allow.iter().copied())
    }

    #[test]
    fn a_connect_head_parses_into_a_method_a_target_and_a_session() {
        let head = "CONNECT crates.io:443 HTTP/1.1\r\nHost: crates.io:443\r\n\
                    Proxy-Authorization: Basic MDEyMzQ1Njc4OWFiY2RlZjp4\r\n\r\n";
        let request = Request::parse(head).expect("the head parses");
        assert_eq!(request.method, "CONNECT");
        assert_eq!(request.target, "crates.io:443");
        // `0123456789abcdef:x` in base64: the username is the session, and the password is not read.
        assert_eq!(request.session.as_deref(), Some("0123456789abcdef"));
        // The same head without the header, and one whose username is not a session id.
        assert_eq!(
            Request::parse("CONNECT a:443 HTTP/1.1\r\n\r\n")
                .expect("a head with no username parses")
                .session,
            None
        );
        let odd =
            "CONNECT a:443 HTTP/1.1\r\nProxy-Authorization: Basic 5L2g5aW977yM5LiW55WM\r\n\r\n";
        assert_eq!(
            Request::parse(odd).expect("a head parses").session,
            None,
            "not a session id"
        );
        let bearer = "CONNECT a:443 HTTP/1.1\r\nProxy-Authorization: Bearer abc\r\n\r\n";
        assert_eq!(
            Request::parse(bearer).expect("a head parses").session,
            None,
            "not Basic"
        );
    }

    #[test]
    fn a_head_that_is_not_an_http_request_does_not_parse() {
        for head in [
            "",
            "CONNECT\r\n\r\n",
            "CONNECT a:443\r\n\r\n",
            "CONNECT a:443 HTTP/1.1 extra\r\n\r\n",
        ] {
            assert!(Request::parse(head).is_none(), "{head:?}");
        }
        // A method that is not CONNECT still *parses*: it is a refusal with a status, not a malformed head,
        // because the answer should tell the caller what this proxy does.
        let get = Request::parse("GET http://a/ HTTP/1.1\r\n\r\n").expect("parses");
        assert_eq!(
            rules(&["a"]).target(&get),
            Err(Refusal::NotTunnel {
                method: "GET".to_string()
            })
        );
    }

    #[test]
    fn the_target_must_be_host_and_port_and_the_port_must_be_443() {
        let rules = rules(&["crates.io"]);
        assert_eq!(
            rules.target(&connect_request("crates.io:443")),
            Ok(Target {
                host: "crates.io".to_string(),
                port: 443
            })
        );
        assert_eq!(
            rules.target(&connect_request("crates.io:80")),
            Err(Refusal::NotTls {
                host: "crates.io".to_string(),
                port: 80
            })
        );
        for target in [
            "crates.io",
            "crates.io:",
            "crates.io:http",
            "/crates.io:443",
            "http://crates.io:443",
            "[::1]",
            "::1:443",
            ":443",
        ] {
            assert!(
                matches!(
                    rules.target(&connect_request(target)),
                    Err(Refusal::Malformed { .. })
                ),
                "{target:?}"
            );
        }
        // An IPv6 literal is spelled with brackets, and it parses: the address rule refuses it later, which is
        // a different answer from "that is not a target".
        assert_eq!(
            rules.target(&connect_request("[2606:4700::1111]:443")),
            Err(Refusal::NotListed {
                host: "2606:4700::1111".to_string()
            })
        );
    }

    #[test]
    fn the_allow_list_is_an_exact_domain_and_not_a_suffix() {
        let list = rules(&["crates.io", "static.crates.io"]);
        assert!(list.permits("crates.io"));
        assert!(list.permits("static.crates.io"));
        // The four ways a suffix rule would be wider than the list said.
        for host in [
            "sub.crates.io",
            "evilcrates.io",
            "crates.io.attacker.example",
            "io",
        ] {
            assert!(!list.permits(host), "{host} must not match");
        }
        // DNS is case-insensitive and a name may carry a root dot; both are the same rule.
        assert!(list.permits("Crates.IO"));
        assert!(list.permits("crates.io."));
        // An empty list permits nothing, which is what `network_default = false` ships as.
        assert!(!rules(&[]).permits("crates.io"));
        assert_eq!(rules(&[]).allow(), &[] as &[String]);
    }

    #[test]
    fn a_host_that_is_not_on_the_list_is_refused_by_name() {
        let refusal = rules(&["crates.io"])
            .target(&connect_request("example.com:443"))
            .expect_err("not listed");
        assert_eq!(
            refusal,
            Refusal::NotListed {
                host: "example.com".to_string()
            }
        );
        assert_eq!(refusal.status(), 403);
        assert_eq!(refusal.word(), "not-listed");
        assert!(refusal.sentence().contains("example.com"), "{}", refusal);
        assert!(refusal.sentence().contains("allow-list"), "{}", refusal);
    }

    #[test]
    fn every_range_item_2_names_is_inside_and_nothing_else_is() {
        let inside = [
            ("127.0.0.1", "loopback"),
            ("127.9.9.9", "loopback"),
            ("::1", "loopback"),
            ("0.0.0.0", "unspecified"),
            ("::", "unspecified"),
            ("169.254.1.1", "link-local"),
            ("fe80::1", "link-local"),
            ("10.1.2.3", "RFC 1918"),
            ("172.16.0.1", "RFC 1918"),
            ("172.31.255.255", "RFC 1918"),
            ("192.168.1.1", "RFC 1918"),
            ("100.64.0.1", "100.64.0.0/10"),
            ("100.127.255.254", "100.64.0.0/10"),
            ("fc00::1", "unique local"),
            ("fd12:3456::1", "unique local"),
            ("fec0::1", "site-local"),
            ("0.0.0.1", "this network"),
            // Three IPv6 spellings that *carry* a v4 destination: on a host with a route for one of them
            // `CONNECT <listed>:443` would reach that v4 address, which is why the check recurses.
            ("2002:7f00:1::1", "loopback"),
            ("64:ff9b::7f00:1", "loopback"),
            ("::7f00:1", "loopback"),
            // The mapped spelling of a loopback address: one spelling deep would let this through.
            ("::ffff:127.0.0.1", "loopback"),
            ("::ffff:10.0.0.1", "RFC 1918"),
        ];
        for (address, range) in inside {
            let address: IpAddr = address.parse().expect("an address");
            let problem = address_problem(address).unwrap_or_else(|| panic!("{address} is inside"));
            assert!(problem.contains(range), "{address}: {problem}");
        }
        for address in [
            "1.1.1.1",
            "8.8.8.8",
            "172.32.0.1",
            "100.63.255.255",
            "100.128.0.1",
            "192.169.0.1",
            "2606:4700:4700::1111",
            "2002:0101:0101::1",
            "64:ff9b::101:101",
        ] {
            let address: IpAddr = address.parse().expect("an address");
            assert_eq!(address_problem(address), None, "{address} is outside");
        }
    }

    #[test]
    fn a_refusal_names_what_it_refused_and_a_tunnel_does_not() {
        let inside = Refusal::Inside {
            host: "localhost".to_string(),
            address: "127.0.0.1".parse().expect("an address"),
            range: "loopback (127.0.0.0/8)",
        };
        assert_eq!(inside.status(), 403);
        assert_eq!(inside.word(), "inside");
        assert_eq!(inside.host(), "localhost");
        assert!(inside.sentence().contains("127.0.0.1"), "{inside}");
        assert!(inside.sentence().contains("allow-list"), "{inside}");

        let method = Refusal::NotTunnel {
            method: "POST".to_string(),
        };
        assert_eq!(method.status(), 405);
        assert_eq!(method.host(), UNATTRIBUTED);
        assert!(method.sentence().contains("CONNECT"), "{method}");

        let tls = Refusal::NotTls {
            host: "crates.io".to_string(),
            port: 80,
        };
        assert_eq!(tls.status(), 403);
        assert!(tls.sentence().contains("443"), "{tls}");

        let unreachable = Refusal::Unreachable {
            host: "crates.io".to_string(),
            detail: "no such host".to_string(),
        };
        assert_eq!(unreachable.status(), 502);
        assert!(
            unreachable.sentence().contains("no such host"),
            "{unreachable}"
        );
    }

    #[test]
    fn a_request_that_sent_no_username_is_logged_as_unattributed() {
        let refusal = Refusal::NotListed {
            host: "example.com".to_string(),
        };
        let record = Record::refused(UNATTRIBUTED, &refusal);
        assert_eq!(record.session, "-");
        assert_eq!(record.host, "example.com");
        assert_eq!(record.status, "not-listed");
        assert_eq!((record.up, record.down), (0, 0));
        // A session id reaches the line as the session field, never as part of the host.
        let attributed = Record::refused("0123456789abcdef", &refusal);
        assert_eq!(attributed.session, "0123456789abcdef");
        assert_eq!(attributed.host, "example.com");
    }

    #[tokio::test]
    async fn a_tunnel_answers_a_200_with_no_body_copies_both_ways_and_counts_the_bytes() {
        // The allow path, without the internet and **below the policy**: the addresses here are loopback, which
        // `address_problem` refuses — so this drives `Record::tunnel`, the function `handle` calls once the
        // policy has said yes. That is deliberate: the `200` a `CONNECT` gets must carry **no body** (the first
        // version answered with a `Content-Length` and `curl` said `wrong version number`, which no unit test
        // covering only the refusals would have caught), and the counts must be the bytes that moved.
        let upstream = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = upstream.local_addr().expect("an address");
        let echo = tokio::spawn(async move {
            let (mut socket, _) = upstream.accept().await.expect("a connection");
            let mut buffer = [0u8; 64];
            loop {
                let read = socket.read(&mut buffer).await.expect("a read");
                if read == 0 {
                    return;
                }
                socket.write_all(&buffer[..read]).await.expect("a write");
            }
        });
        let proxy = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let client_address = proxy.local_addr().expect("an address");
        let tunnel = tokio::spawn(async move {
            let (stream, _) = proxy.accept().await.expect("a connection");
            Record::tunnel(
                stream,
                "example.com",
                "abcdef0123456789".to_string(),
                &[address],
            )
            .await
        });

        let mut client = TcpStream::connect(client_address)
            .await
            .expect("the tunnel");
        let mut head = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            client.read_exact(&mut byte).await.expect("a read");
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        assert_eq!(
            String::from_utf8(head).expect("ascii"),
            "HTTP/1.1 200 Connection Established\r\n\r\n",
            "a tunnel's answer is the status line and a blank line, and nothing after it"
        );

        client.write_all(b"hello egress").await.expect("a write");
        let mut answer = [0u8; 12];
        client.read_exact(&mut answer).await.expect("a read");
        assert_eq!(&answer, b"hello egress");
        drop(client);
        let record = tunnel.await.expect("the tunnel ends");
        assert_eq!(record.status, "allowed");
        assert_eq!(record.session, "abcdef0123456789");
        assert_eq!(record.host, "example.com");
        assert_eq!(
            (record.up, record.down),
            (12, 12),
            "both directions are counted"
        );
        echo.abort();
    }

    #[tokio::test]
    async fn a_tunnel_nothing_answers_is_a_502_and_a_record_with_no_bytes() {
        let proxy = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let client_address = proxy.local_addr().expect("an address");
        // A port nothing listens on: the tunnel is refused *after* the policy said yes, which is the "reachable
        // name, nothing behind it" case a container meets when its allow-listed host is down.
        //
        // **Port 0, because nothing can ever listen on it** — a `bind` with 0 asks the kernel for an ephemeral port,
        // so no process holds port 0 — and a connect to it is refused at once. This was a bound-and-dropped ephemeral
        // port first, which is a race: another test in this binary binding that port between the drop and the
        // connect makes the connect *succeed* against a listener that never answers, and the read below waits
        // forever. It cost the gate run fourteen minutes of a hung lib-test binary (`wchan` was
        // `skb_wait_for_more_packets`), so the port is deterministic now and the read is bounded.
        let dead_address = SocketAddr::from(([127, 0, 0, 1], 0));
        let tunnel = tokio::spawn(async move {
            let (stream, _) = proxy.accept().await.expect("a connection");
            Record::tunnel(
                stream,
                "example.com",
                UNATTRIBUTED.to_string(),
                &[dead_address],
            )
            .await
        });
        let mut client = TcpStream::connect(client_address).await.expect("the proxy");
        let mut answer = String::new();
        tokio::time::timeout(Duration::from_secs(10), client.read_to_string(&mut answer))
            .await
            .expect("the refused tunnel is answered and closed; a hang here is a gate that cannot finish")
            .expect("a read");
        assert!(answer.starts_with("HTTP/1.1 502"), "{answer}");
        let record = tunnel.await.expect("the tunnel ends");
        assert_eq!(record.status, "unreachable");
        assert_eq!((record.up, record.down), (0, 0));
    }

    #[tokio::test]
    async fn the_wire_refuses_what_the_rules_refuse() {
        // The thin layer, driven as a client: a real socket, a real head, the real answer. Nothing here needs a
        // daemon, the internet or a name that resolves.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = listener.local_addr().expect("an address");
        let rules = Arc::new(rules(&["localhost", "crates.io"]));
        let server = tokio::spawn(async move {
            for _ in 0..4 {
                let (stream, _) = listener.accept().await.expect("a connection");
                let rules = Arc::clone(&rules);
                tokio::spawn(async move { handle(stream, &rules).await });
            }
        });
        let ask = |head: &'static str| async move {
            let mut stream = TcpStream::connect(address).await.expect("the proxy");
            stream.write_all(head.as_bytes()).await.expect("a write");
            let mut answer = String::new();
            stream.read_to_string(&mut answer).await.expect("a read");
            answer
        };
        // A method that is not CONNECT: 405, and the sentence says what the proxy does.
        let answer = ask("GET http://crates.io/ HTTP/1.1\r\nHost: crates.io\r\n\r\n").await;
        assert!(answer.starts_with("HTTP/1.1 405"), "{answer}");
        assert!(answer.contains("CONNECT"), "{answer}");
        // A port that is not 443, before the name is even looked at.
        let answer = ask("CONNECT crates.io:80 HTTP/1.1\r\n\r\n").await;
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        assert!(answer.contains("443"), "{answer}");
        // A host that is not on the list.
        let answer = ask("CONNECT example.com:443 HTTP/1.1\r\n\r\n").await;
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        assert!(answer.contains("example.com"), "{answer}");
        // **In** the allow-list and still refused: `localhost` resolves to a loopback address, and item 2 says
        // the address rule wins. This is the case that would pass if the check were skipped for listed names.
        let answer = ask("CONNECT localhost:443 HTTP/1.1\r\n\r\n").await;
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        assert!(answer.contains("127.0.0.1"), "{answer}");
        assert!(answer.contains("allow-list"), "{answer}");
        server.abort();
    }
}
