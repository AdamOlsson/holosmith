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
//! 4. **Forward over TLS and stream back.** [`crate::relay`] is that half and the MCP proxy's too: `openssl
//!    s_client` is the TLS client because the artifact is static and has no TLS stack, and response bytes are
//!    copied as they arrive, so an SSE body is not buffered behind a `Content-Length` that never comes.
//!
//! **The network is the first half of "only session networks".** The container publishes no port, listens on
//! `0.0.0.0:8080`, and is joined to the machine's `egress` network and every session's `agents_<id>` network with
//! the alias `key-proxy`. A dummy key is the second half: a request from the wrong network still needs one. No
//! address list is read from Docker here, and the ADR says why that is enough.
//!
//! **The rule is this module's; the transport is [`crate::relay`]'s.** [`Route::parse`] and [`upstream`] decide
//! from values and can be tested without a socket.

use std::fmt;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use crate::key::{self, Auth, Provider, Store};
use crate::relay::{self, Head, Relayed};

/// Where the proxy listens unless `--listen` says otherwise.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:8080";

/// One request selected by its path, before the key is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub provider: &'static Provider,
    /// The header this request's shape carries the key in, selected from the endpoint pi appended. It is the
    /// row's own [`auth`](Provider::auth) for every request that matches no [`Shape`](crate::key::Shape).
    pub auth: Auth,
    /// The path the provider's API expects, with the key proxy's `/<provider>` prefix removed.
    pub path: String,
}

impl Route {
    /// Parse an origin-form request target (`/anthropic/v1/messages?beta=…`).
    ///
    /// The provider segment is matched exactly against [`key::PROVIDERS`]. A path that does not start with
    /// `/`, a provider this build does not know, or a `..` segment is refused with a sentence: there is no
    /// fallback host to send it to.
    ///
    /// **The tail selects the shape.** What pi appended after `/\<provider\>` is the endpoint its bundled
    /// catalog says the model speaks, and a row that serves more than one API names a base path and a header
    /// per endpoint ([`Provider::shape_for`]). A row without shapes, and a tail a row with shapes does not
    /// name, keep the row's own path and header, so this is the one place a shape is chosen and everything
    /// else about routing is unchanged.
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
        // The endpoint as a path: the split dropped the `/` after the provider id, and a shape names
        // `/v1/messages`, the spelling pi and the provider both use. The forwarded path is the shape's base path
        // and then this.
        let endpoint = if tail.is_empty() {
            String::new()
        } else {
            format!("/{tail}")
        };
        let (base_path, auth) = provider.shape_for(&endpoint);
        let mut path = format!("{base_path}{endpoint}");
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
        Ok(Self {
            provider,
            auth,
            path,
        })
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

/// One parsed request: the transport's head and this proxy's route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub head: Head,
    pub route: Route,
}

impl Request {
    /// The session's dummy key, from either header pi's providers use.
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

/// Build the request that goes over TLS: the provider's path, the real key in the header its shape uses, the body
/// the client sent, and only headers that mean the same thing upstream.
///
/// The shape of the bytes is [`relay::Upstream::request`]'s; what this adds is the one place the key proxy's route
/// becomes it. The header the key goes in is the **route's**, not the row's: the endpoint pi appended selected the
/// shape, and a shape whose client sends `Authorization: Bearer` carries no `x-api-key` even when the row's own
/// default is Anthropic's.
pub fn upstream(request: &Request, real_key: &str, body: &[u8]) -> Vec<u8> {
    relay::Upstream {
        method: &request.head.method,
        host: request.route.provider.host,
        path: &request.route.path,
        auth: request.route.auth,
    }
    .request(&request.head, real_key, body)
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
    relay::serve(listener, "key proxy", move |stream| {
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
    let provider = request.route.provider;
    let secrets = match store.load() {
        Ok(secrets) => secrets,
        Err(error) => {
            let _ = relay::respond(
                &mut stream,
                503,
                &format!("the key store could not be read: {error}"),
            )
            .await;
            Record::refused("-", provider.id, provider.host, "store").log();
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
        Record::refused("-", provider.id, provider.host, "no-key").log();
        return;
    };
    let Some(session) = Store::session_of(&secrets, dummy) else {
        let _ = relay::respond(
            &mut stream,
            401,
            "that dummy key is not valid for any session",
        )
        .await;
        Record::refused("-", provider.id, provider.host, "bad-key").log();
        return;
    };
    let Some(real) = secrets.providers.get(provider.id) else {
        let _ = relay::respond(
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
    if let Some(value) = request.head.content_length
        && value > relay::MAX_BODY
    {
        let _ = relay::respond(
            &mut stream,
            413,
            "the request body is larger than this proxy accepts",
        )
        .await;
        Record::refused(session, provider.id, provider.host, "too-large").log();
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
            Record::refused(session, provider.id, provider.host, "bad-body").log();
            return;
        }
    };
    let answered = upstream(&request, real, &body);
    forward(
        &mut stream,
        session,
        provider.id,
        provider.host,
        &answered,
        body.len(),
    )
    .await;
}

/// Forward `request` to the route's host over TLS and stream the answer back, logging one record.
async fn forward(
    stream: &mut TcpStream,
    session: &str,
    provider: &'static str,
    host: &'static str,
    request: &[u8],
    body: usize,
) {
    match relay::forward(stream, host, request).await {
        Relayed::Answered(down) => Record {
            session: session.to_string(),
            provider,
            host,
            status: "forwarded",
            up: body as u64,
            down,
        }
        .log(),
        Relayed::ClientClosed(down) => Record {
            session: session.to_string(),
            provider,
            host,
            status: "client-closed",
            up: body as u64,
            down,
        }
        .log(),
        Relayed::Failed(failure) => {
            let _ = relay::respond(stream, 502, &failure.sentence()).await;
            Record::refused(session, provider, host, failure.word()).log();
        }
    }
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

    #[test]
    fn an_opencode_go_route_takes_its_shape_from_the_endpoint_pi_appended() {
        // One row, two APIs under one host, and the model's shape is what pi appends: an
        // `anthropic-messages` model sends `/v1/messages` to `/inference/go/anthropic` with `x-api-key`, and an
        // `openai-completions` model sends `/chat/completions` to `/inference/go/openai/v1` with `Bearer`. The
        // defect: the row's own anthropic prefix was used for both, so every openai-completions model was
        // forwarded to `/inference/go/anthropic/chat/completions` and answered `404 status code (no body)`.
        let anthropic = Route::parse("/opencode-go/v1/messages").expect("a route");
        assert_eq!(anthropic.provider.host, "opencode.ai");
        assert_eq!(anthropic.path, "/inference/go/anthropic/v1/messages");
        assert_eq!(anthropic.auth, Auth::XApiKey);
        let parsed = parse(&head("/opencode-go/v1/messages")).expect("a head");
        let text = String::from_utf8(upstream(&parsed, "sk-real", b"{}")).expect("utf-8");
        assert!(text.starts_with("POST /inference/go/anthropic/v1/messages HTTP/1.1\r\n"));
        assert!(text.contains("Host: opencode.ai\r\n"), "{text}");
        assert!(text.contains("x-api-key: sk-real\r\n"), "{text}");
        assert!(!text.contains("Authorization:"), "{text}");

        let openai = Route::parse("/opencode-go/chat/completions").expect("a route");
        assert_eq!(openai.provider.host, "opencode.ai");
        assert_eq!(openai.path, "/inference/go/openai/v1/chat/completions");
        assert_eq!(openai.auth, Auth::Bearer);
        let parsed = parse(&head("/opencode-go/chat/completions")).expect("a head");
        let text = String::from_utf8(upstream(&parsed, "sk-real", b"{}")).expect("utf-8");
        assert!(
            text.starts_with("POST /inference/go/openai/v1/chat/completions HTTP/1.1\r\n"),
            "{text}"
        );
        assert!(text.contains("Authorization: Bearer sk-real\r\n"), "{text}");
        assert!(
            !text.contains("x-api-key"),
            "the shape's header is the only one that carries a key: {text}"
        );
    }

    #[test]
    fn a_query_string_survives_the_shape_s_rewrite() {
        let route = Route::parse("/opencode-go/chat/completions?stream=true").expect("a route");
        assert_eq!(
            route.path,
            "/inference/go/openai/v1/chat/completions?stream=true"
        );
        let route = Route::parse("/anthropic/v1/messages?beta=1").expect("a route");
        assert_eq!(route.path, "/v1/messages?beta=1");
    }

    #[test]
    fn a_row_without_shapes_keeps_its_own_path_and_header_whatever_the_endpoint() {
        for (target, path, auth) in [
            ("/anthropic/v1/messages", "/v1/messages", Auth::XApiKey),
            (
                "/anthropic/chat/completions",
                "/chat/completions",
                Auth::XApiKey,
            ),
            (
                "/openai/chat/completions",
                "/v1/chat/completions",
                Auth::Bearer,
            ),
            (
                "/groq/chat/completions",
                "/openai/v1/chat/completions",
                Auth::Bearer,
            ),
        ] {
            let route = Route::parse(target).expect("a route");
            assert_eq!(route.path, path, "{target}");
            assert_eq!(route.auth, auth, "{target}");
        }
    }

    #[test]
    fn a_dummy_key_never_survives_into_the_upstream_request() {
        // The client's header is stripped whatever shape selected the request and whatever header it carried: pi
        // sends the dummy in `x-api-key` for one shape and `Authorization: Bearer` for the other, and the
        // upstream request must carry the real key in the selected shape's header and nothing else.
        for target in [
            "/opencode-go/v1/messages",
            "/opencode-go/chat/completions",
            "/anthropic/v1/messages",
        ] {
            for header in ["x-api-key: hs-dummy-x", "Authorization: Bearer hs-dummy-y"] {
                let parsed = parse(&format!(
                    "POST {target} HTTP/1.1\r\nHost: key-proxy:8080\r\n{header}\r\n\r\n"
                ))
                .expect("a head");
                let text = String::from_utf8(upstream(&parsed, "sk-real", b"")).expect("utf-8");
                assert!(!text.contains("hs-dummy"), "{target}: {text}");
                assert!(text.contains("sk-real"), "{target}: {text}");
                assert_eq!(
                    text.matches("sk-real").count(),
                    1,
                    "the key must appear once, in one header: {target}: {text}"
                );
            }
        }
    }

    #[test]
    fn a_dummy_key_is_read_from_either_header_and_never_echoed() {
        let parsed = parse(&head("/anthropic/v1/messages")).expect("a head");
        assert_eq!(parsed.dummy(), Some("hs-dummy-x"));
        let bearer =
            "POST /openai/chat/completions HTTP/1.1\r\nAuthorization: Bearer hs-dummy-y\r\n\r\n";
        assert_eq!(parse(bearer).expect("a head").dummy(), Some("hs-dummy-y"));
    }

    #[test]
    fn a_head_that_is_not_http_is_a_route_error_with_the_heads_own_words() {
        let error = parse("CONNECT example.com:443 HTTP/1.1\r\n\r\n").expect_err("no route");
        assert!(matches!(error, RouteError::NotAPath(_)), "{error}");
        assert_eq!(error.status(), 400);
    }
}
