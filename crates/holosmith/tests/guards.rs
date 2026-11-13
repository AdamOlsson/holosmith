//! The browser-facing guards as a client meets them (ADR-0010).
//!
//! Every test starts the real `App` on an ephemeral port and talks HTTP/1.1 over a socket, so
//! each answer is the one a browser gets. The configs are built by field assignment rather
//! than through a config file: the guards must be testable for a machine that never writes one.

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::{Method, header};
use holosmith::config::{Config, Origin};
use holosmith::http::error::{BAD_CLIENT, BAD_HOST, BAD_ORIGIN, BAD_OWNER, NOT_FOUND};
use holosmith::http::{self, Guards, RouteKind, Routes};

mod support;
use support::{Response, Served, request, state};

/// `Config::default().control_origin`, which is what the default guards expect.
const ORIGIN: &str = "http://localhost:8080";

/// The host the harness's own requests carry.
const LOOPBACK: &str = "127.0.0.1";

/// A login a test configures as the owner.
const OWNER: &str = "owner@example";

const GRACE: Duration = Duration::from_secs(10);

/// Every header a request that changes something needs.
const MUTATING_HEADERS: &[(&str, &str)] = &[
    ("Origin", ORIGIN),
    ("Content-Type", "application/json"),
    ("X-Holosmith-Client", "1"),
];

fn guards() -> Guards {
    Guards::from_config(&Config::default())
}

fn guards_for(hosts: &[&str], owner: Option<&str>, origin: &str) -> Guards {
    Guards::from_config(&Config {
        allowed_hosts: hosts.iter().map(|host| (*host).to_string()).collect(),
        owner: owner.map(str::to_string),
        control_origin: Origin::parse(origin).expect("a test origin is a valid origin"),
        ..Config::default()
    })
}

/// The envelope's `code`, named from the constants rather than a literal by every caller.
fn body_code(response: &Response) -> String {
    let body: serde_json::Value =
        serde_json::from_str(&response.body).expect("an envelope is a JSON object");
    body.get("code")
        .and_then(|code| code.as_str())
        .expect("the envelope names a code")
        .to_string()
}

/// A request with the headers a test names and `Connection: close`, which no browser sends.
async fn send(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)]) -> Response {
    let mut text = format!("{method} {path} HTTP/1.1\r\n");
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str("Connection: close\r\n\r\n");
    request(addr, &text).await
}

/// A body-less `POST`, which still needs the JSON content type (D5).
async fn post(addr: SocketAddr, path: &str, extra_headers: &[(&str, &str)]) -> Response {
    let mut headers = vec![("Host", LOOPBACK), ("Content-Length", "0")];
    headers.extend_from_slice(extra_headers);
    send(addr, "POST", path, &headers).await
}

/// Whether an answer carries a CORS header or a `Vary: Origin` that would imply one.
fn cors_free(response: &Response) -> bool {
    let no_cors = response
        .headers
        .iter()
        .all(|(name, _)| !name.to_ascii_lowercase().starts_with("access-control-"));
    let no_vary_origin = response.header("vary").is_none_or(|vary| {
        !vary
            .to_ascii_lowercase()
            .split(',')
            .any(|value| value.trim() == "origin")
    });
    no_cors && no_vary_origin
}

// ---- the Host allow-list -------------------------------------------------------------

#[tokio::test]
async fn a_host_outside_the_allow_list_is_bad_host() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = send(served.addr, "GET", "/api/nope", &[("Host", "evil.example")]).await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_HOST);
    assert_eq!(response.header("content-type"), Some("application/json"));
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn localhost_and_loopback_are_implicit() {
    let served = Served::start(http::router(state()), GRACE).await;
    for host in ["localhost", LOOPBACK] {
        let response = send(served.addr, "GET", "/api/nope", &[("Host", host)]).await;
        assert_eq!(response.status, 404, "{host} must be implicit");
        assert_eq!(body_code(&response), NOT_FOUND, "{host}");
    }
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_configured_host_is_accepted_case_insensitively() {
    let served = Served::start_with(
        http::router(state()),
        guards_for(&["control.example"], None, ORIGIN),
        GRACE,
    )
    .await;
    let accepted = send(
        served.addr,
        "GET",
        "/api/nope",
        &[("Host", "CONTROL.Example:8443")],
    )
    .await;
    assert_eq!(accepted.status, 404);
    assert_eq!(body_code(&accepted), NOT_FOUND);
    let refused = send(
        served.addr,
        "GET",
        "/api/nope",
        &[("Host", "other.example")],
    )
    .await;
    assert_eq!(refused.status, 403);
    assert_eq!(body_code(&refused), BAD_HOST);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_host_check_ignores_the_port() {
    // The server is on an ephemeral port, and the header names another one.
    let served = Served::start(http::router(state()), GRACE).await;
    let response = send(
        served.addr,
        "GET",
        "/api/nope",
        &[("Host", "127.0.0.1:8080")],
    )
    .await;
    assert_eq!(response.status, 404);
    assert_eq!(body_code(&response), NOT_FOUND);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_bracketed_ipv6_host_must_be_listed() {
    let defaults = Served::start(http::router(state()), GRACE).await;
    let refused = send(defaults.addr, "GET", "/api/nope", &[("Host", "[::1]:8080")]).await;
    assert_eq!(refused.status, 403);
    assert_eq!(body_code(&refused), BAD_HOST);
    defaults.finish().await.expect("a clean shutdown");

    let listed = Served::start_with(
        http::router(state()),
        guards_for(&["[::1]"], None, ORIGIN),
        GRACE,
    )
    .await;
    let accepted = send(listed.addr, "GET", "/api/nope", &[("Host", "[::1]:8080")]).await;
    assert_eq!(accepted.status, 404);
    assert_eq!(body_code(&accepted), NOT_FOUND);
    listed.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_missing_host_is_bad_host() {
    // hyper 1.12's h1 parser has no `Host` requirement (measured in ADR-0010), so this is a
    // check the guard performs rather than one the stack performs for it.
    let served = Served::start(http::router(state()), GRACE).await;
    let response = request(
        served.addr,
        "GET /api/nope HTTP/1.1\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_HOST);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn two_host_headers_are_bad_host() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = send(
        served.addr,
        "GET",
        "/api/nope",
        &[("Host", LOOPBACK), ("Host", "evil.example")],
    )
    .await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_HOST);
    served.finish().await.expect("a clean shutdown");
}

// ---- the control origin --------------------------------------------------------------

#[tokio::test]
async fn a_non_get_without_an_origin_is_bad_origin() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = post(served.addr, "/api/nope", &[]).await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_ORIGIN);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_non_get_from_another_origin_is_bad_origin() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = post(
        served.addr,
        "/api/nope",
        &[("Origin", "https://evil.example")],
    )
    .await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_ORIGIN);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn origin_null_is_bad_origin() {
    // A sandboxed frame or a `file://` page sends `Origin: null`, which no configured origin is.
    let served = Served::start(http::router(state()), GRACE).await;
    let response = post(served.addr, "/api/nope", &[("Origin", "null")]).await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_ORIGIN);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn two_origin_headers_are_bad_origin() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = post(
        served.addr,
        "/api/nope",
        &[("Origin", ORIGIN), ("Origin", "https://evil.example")],
    )
    .await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_ORIGIN);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_configured_control_origin_passes() {
    let served = Served::start_with(
        http::router(state()),
        guards_for(&[], None, "https://Control.Example:8443"),
        GRACE,
    )
    .await;
    // A browser sends the origin lowercase; the config's spelling is not.
    let response = post(
        served.addr,
        "/api/nope",
        &[
            ("Origin", "https://control.example:8443"),
            ("Content-Type", "application/json"),
            ("X-Holosmith-Client", "1"),
        ],
    )
    .await;
    assert_eq!(response.status, 404, "{}", response.body);
    assert_eq!(body_code(&response), NOT_FOUND);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_loopback_origin_is_accepted_in_either_spelling() {
    // A bookmark by IP and a page loaded from `localhost` are one listener, so the guard accepts
    // whichever of the two the config does not spell — and either way round.
    let cases = [
        ("http://localhost:8080", "http://127.0.0.1:8080"),
        ("http://127.0.0.1:8080", "http://localhost:8080"),
    ];
    for (configured, sent) in cases {
        let served = Served::start_with(
            http::router(state()),
            guards_for(&[], None, configured),
            GRACE,
        )
        .await;
        let response = post(
            served.addr,
            "/api/nope",
            &[
                ("Origin", sent),
                ("Content-Type", "application/json"),
                ("X-Holosmith-Client", "1"),
            ],
        )
        .await;
        assert_eq!(
            response.status, 404,
            "{sent} against a {configured} control origin: {}",
            response.body
        );
        assert_eq!(body_code(&response), NOT_FOUND);
        served.finish().await.expect("a clean shutdown");
    }
}

#[tokio::test]
async fn a_spelling_the_alias_rule_does_not_cover_is_bad_origin() {
    // The alias rule is a loopback rule: a real domain is one origin, so its refusal is the one the refusing case
    // above asserts. The two other axes are the ones the gate leans on — the peer must refuse another loopback
    // *port*, and another scheme on the same loopback host — so they are pinned here rather than only in the gate's
    // relay arm.
    let cases = [
        ("https://control.example:8443", "https://127.0.0.1:8443"),
        (ORIGIN, "http://127.0.0.1:8081"),
        (ORIGIN, "https://127.0.0.1:8080"),
    ];
    for (configured, sent) in cases {
        let served = Served::start_with(
            http::router(state()),
            guards_for(&[], None, configured),
            GRACE,
        )
        .await;
        let response = post(
            served.addr,
            "/api/nope",
            &[
                ("Origin", sent),
                ("Content-Type", "application/json"),
                ("X-Holosmith-Client", "1"),
            ],
        )
        .await;
        assert_eq!(
            response.status, 403,
            "{sent} against a {configured} control origin: {}",
            response.body
        );
        assert_eq!(body_code(&response), BAD_ORIGIN);
        served.finish().await.expect("a clean shutdown");
    }
}

#[tokio::test]
async fn a_get_needs_no_origin() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = send(served.addr, "GET", "/api/nope", &[("Host", LOOPBACK)]).await;
    assert_eq!(response.status, 404);
    assert_eq!(body_code(&response), NOT_FOUND);
    served.finish().await.expect("a clean shutdown");
}

// ---- the JSON client header ----------------------------------------------------------

#[tokio::test]
async fn a_non_get_without_the_client_header_is_bad_client() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = post(
        served.addr,
        "/api/nope",
        &[("Origin", ORIGIN), ("Content-Type", "application/json")],
    )
    .await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_CLIENT);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_non_get_without_a_json_content_type_is_bad_client() {
    let served = Served::start(http::router(state()), GRACE).await;
    let form = post(
        served.addr,
        "/api/nope",
        &[
            ("Origin", ORIGIN),
            ("Content-Type", "text/plain"),
            ("X-Holosmith-Client", "1"),
        ],
    )
    .await;
    assert_eq!(form.status, 403);
    assert_eq!(body_code(&form), BAD_CLIENT);
    // A body-less `POST` needs both headers too: a form can be posted with no body.
    let bare = post(served.addr, "/api/nope", &[("Origin", ORIGIN)]).await;
    assert_eq!(bare.status, 403);
    assert_eq!(body_code(&bare), BAD_CLIENT);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_json_suffix_is_not_json() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = post(
        served.addr,
        "/api/nope",
        &[
            ("Origin", ORIGIN),
            ("Content-Type", "application/json+something"),
            ("X-Holosmith-Client", "1"),
        ],
    )
    .await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_CLIENT);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_client_header_must_be_exactly_one() {
    let served = Served::start(http::router(state()), GRACE).await;
    for value in ["2", "true", "1x", ""] {
        let response = post(
            served.addr,
            "/api/nope",
            &[
                ("Origin", ORIGIN),
                ("Content-Type", "application/json"),
                ("X-Holosmith-Client", value),
            ],
        )
        .await;
        assert_eq!(response.status, 403, "the client header must be {value:?}");
        assert_eq!(body_code(&response), BAD_CLIENT, "{value:?}");
    }
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn two_client_headers_are_bad_client() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = post(
        served.addr,
        "/api/nope",
        &[
            ("Origin", ORIGIN),
            ("Content-Type", "application/json"),
            ("X-Holosmith-Client", "1"),
            ("X-Holosmith-Client", "1"),
        ],
    )
    .await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_CLIENT);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn two_content_type_headers_are_bad_client() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = post(
        served.addr,
        "/api/nope",
        &[
            ("Origin", ORIGIN),
            ("Content-Type", "application/json"),
            ("Content-Type", "text/plain"),
            ("X-Holosmith-Client", "1"),
        ],
    )
    .await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_CLIENT);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_json_content_type_with_a_parameter_is_accepted() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = post(
        served.addr,
        "/api/nope",
        &[
            ("Origin", ORIGIN),
            ("Content-Type", "application/json; charset=utf-8"),
            ("X-Holosmith-Client", "1"),
        ],
    )
    .await;
    assert_eq!(response.status, 404, "{}", response.body);
    assert_eq!(body_code(&response), NOT_FOUND);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_json_post_with_the_client_header_reaches_the_router() {
    let served = Served::start(http::router(state()), GRACE).await;
    let mut headers = vec![("Host", LOOPBACK)];
    headers.extend_from_slice(MUTATING_HEADERS);
    let response = send(served.addr, "POST", "/api/nope", &headers).await;
    assert_eq!(response.status, 404, "{}", response.body);
    assert_eq!(body_code(&response), NOT_FOUND);
    served.finish().await.expect("a clean shutdown");
}

// ---- no CORS header, ever ------------------------------------------------------------

/// One answer in the CORS sweep: what it is, the request, and the status and envelope code it
/// must get. The code is `None` for an answer whose body is not an envelope.
type CorsCase = (
    &'static str,
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    u16,
    Option<&'static str>,
);

#[tokio::test]
async fn no_answer_carries_a_cors_header() {
    // One server with an owner configured, so every 403 the guard can send is reachable here.
    let served = Served::start_with(
        http::router(state()),
        guards_for(&[], Some(OWNER), ORIGIN),
        GRACE,
    )
    .await;
    // One row per answer: what it is, the request, the status, and the code when the body is
    // an envelope. A single array, so a case cannot be added without running, and the code is
    // asserted wherever there is one.
    let cases: [CorsCase; 9] = [
        (
            "the health probe",
            "GET",
            "/api/health",
            &[("Host", LOOPBACK)],
            200,
            None,
        ),
        (
            "the app shell",
            "GET",
            "/",
            &[("Host", LOOPBACK)],
            200,
            None,
        ),
        (
            "an asset the app does not ship",
            "GET",
            "/assets/absent.js",
            &[("Host", LOOPBACK)],
            200,
            None,
        ),
        (
            "an unknown path",
            "GET",
            "/api/nope",
            &[("Host", LOOPBACK)],
            404,
            Some(NOT_FOUND),
        ),
        (
            "bad_host",
            "GET",
            "/api/nope",
            &[("Host", "evil.example")],
            403,
            Some(BAD_HOST),
        ),
        (
            "bad_origin",
            "POST",
            "/api/nope",
            // No `Origin` at all: the row this label describes.
            &[("Host", LOOPBACK)],
            403,
            Some(BAD_ORIGIN),
        ),
        (
            "bad_client",
            "POST",
            "/api/nope",
            &[
                ("Host", LOOPBACK),
                ("Origin", ORIGIN),
                ("Content-Type", "application/json"),
            ],
            403,
            Some(BAD_CLIENT),
        ),
        (
            "bad_owner",
            "POST",
            "/api/nope",
            &[
                ("Host", LOOPBACK),
                ("Origin", ORIGIN),
                ("Content-Type", "application/json"),
                ("X-Holosmith-Client", "1"),
                ("Tailscale-User-Login", "someone@example"),
            ],
            403,
            Some(BAD_OWNER),
        ),
        (
            "a preflight",
            "OPTIONS",
            "/api/health",
            &[("Host", LOOPBACK), ("Origin", ORIGIN)],
            403,
            Some(BAD_CLIENT),
        ),
    ];
    for (label, method, path, headers, status, code) in cases {
        let response = send(served.addr, method, path, headers).await;
        assert_eq!(response.status, status, "{label}: {method} {path}");
        if let Some(code) = code {
            assert_eq!(body_code(&response), code, "{label}: {method} {path}");
        }
        assert!(
            cors_free(&response),
            "{label}: {method} {path} carried a CORS header: {:?}",
            response.headers
        );
    }
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_handler_that_adds_a_cors_header_has_it_stripped() {
    // Deliberate, and the one place where code an author wrote has no effect (ADR-0010).
    let routes = Routes::new().get("/api/probe", || async {
        ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], "ok")
    });
    let served = Served::start(routes, GRACE).await;
    let response = send(served.addr, "GET", "/api/probe", &[("Host", LOOPBACK)]).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "ok");
    assert_eq!(response.header("access-control-allow-origin"), None);
    assert!(cors_free(&response), "{:?}", response.headers);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_preflight_is_403_with_no_cors_headers() {
    // `OPTIONS` is not `GET`, so the guard answers it instead of the `405` axum's method router
    // used to send (measured in ADR-0010). Measured again while writing this: axum attaches the
    // route's `Allow` header *outside* the layer, so the 403 for a known path still carries
    // `allow: GET,HEAD`. ADR-0010's consequence is that the status changes to 403 and that no
    // CORS header appears either way; it does not ask for `Allow` to be removed, and removing it
    // would need a second layer or an `Allow` strip this story does not own.
    let served = Served::start(http::router(state()), GRACE).await;
    let response = send(
        served.addr,
        "OPTIONS",
        "/api/health",
        &[("Host", LOOPBACK), ("Origin", ORIGIN)],
    )
    .await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_CLIENT);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert!(cors_free(&response), "{:?}", response.headers);
    served.finish().await.expect("a clean shutdown");
}

// ---- the owner -----------------------------------------------------------------------

#[tokio::test]
async fn an_owner_mismatch_is_bad_owner() {
    // Every earlier check is correct, so the code that comes back is the owner's.
    let served = Served::start_with(
        http::router(state()),
        guards_for(&[], Some(OWNER), ORIGIN),
        GRACE,
    )
    .await;
    let mut headers = vec![("Host", LOOPBACK)];
    headers.extend_from_slice(MUTATING_HEADERS);
    headers.push(("Tailscale-User-Login", "someone@example"));
    let response = send(served.addr, "POST", "/api/nope", &headers).await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_OWNER);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_matching_owner_passes() {
    let served = Served::start_with(
        http::router(state()),
        guards_for(&[], Some(OWNER), ORIGIN),
        GRACE,
    )
    .await;
    let mut headers = vec![("Host", LOOPBACK)];
    headers.extend_from_slice(MUTATING_HEADERS);
    headers.push(("Tailscale-User-Login", OWNER));
    let response = send(served.addr, "POST", "/api/nope", &headers).await;
    assert_eq!(response.status, 404, "{}", response.body);
    assert_eq!(body_code(&response), NOT_FOUND);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn an_absent_login_is_allowed() {
    // Direct access, not through `tailscale serve`: there is no login to compare.
    let served = Served::start_with(
        http::router(state()),
        guards_for(&[], Some(OWNER), ORIGIN),
        GRACE,
    )
    .await;
    let mut headers = vec![("Host", LOOPBACK)];
    headers.extend_from_slice(MUTATING_HEADERS);
    let response = send(served.addr, "POST", "/api/nope", &headers).await;
    assert_eq!(response.status, 404, "{}", response.body);
    assert_eq!(body_code(&response), NOT_FOUND);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn no_owner_configured_means_no_owner_check() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = send(
        served.addr,
        "GET",
        "/api/nope",
        &[
            ("Host", LOOPBACK),
            ("Tailscale-User-Login", "someone@example"),
        ],
    )
    .await;
    assert_eq!(response.status, 404);
    assert_eq!(body_code(&response), NOT_FOUND);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_owner_check_also_applies_to_a_get() {
    // The owner check is about who is calling, not what the method does.
    let served = Served::start_with(
        http::router(state()),
        guards_for(&[], Some(OWNER), ORIGIN),
        GRACE,
    )
    .await;
    let response = send(
        served.addr,
        "GET",
        "/api/nope",
        &[
            ("Host", LOOPBACK),
            ("Tailscale-User-Login", "someone@example"),
        ],
    )
    .await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_OWNER);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn surrounding_whitespace_in_the_login_is_ignored() {
    let served = Served::start_with(
        http::router(state()),
        guards_for(&[], Some(OWNER), ORIGIN),
        GRACE,
    )
    .await;
    let response = send(
        served.addr,
        "GET",
        "/api/nope",
        &[
            ("Host", LOOPBACK),
            ("Tailscale-User-Login", " owner@example "),
        ],
    )
    .await;
    assert_eq!(response.status, 404, "{}", response.body);
    assert_eq!(body_code(&response), NOT_FOUND);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn two_login_headers_are_bad_owner() {
    let served = Served::start_with(
        http::router(state()),
        guards_for(&[], Some(OWNER), ORIGIN),
        GRACE,
    )
    .await;
    let response = send(
        served.addr,
        "GET",
        "/api/nope",
        &[
            ("Host", LOOPBACK),
            ("Tailscale-User-Login", OWNER),
            ("Tailscale-User-Login", "someone@example"),
        ],
    )
    .await;
    assert_eq!(response.status, 403);
    assert_eq!(body_code(&response), BAD_OWNER);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_health_probe_is_exempt_from_the_owner_check() {
    // `is_guard_exempt` is the one exemption list, and an exempt request skips all four checks.
    // So the probe answers from a foreign `Host` and a foreign login: accepted, because the
    // payload is name, version, role and uptime and the probe is what `systemd`, `curl` and a
    // hub's host registration use (ADR-0010).
    let served = Served::start_with(
        http::router(state()),
        guards_for(&[], Some(OWNER), ORIGIN),
        GRACE,
    )
    .await;
    let response = send(
        served.addr,
        "GET",
        "/api/health",
        &[
            ("Host", "evil.example"),
            ("Tailscale-User-Login", "someone@example"),
        ],
    )
    .await;
    assert_eq!(response.status, 200);
    served.finish().await.expect("a clean shutdown");
}

// ---- a `GET` route cannot be mutating ------------------------------------------------

#[test]
fn no_get_route_is_mutating() {
    let app = http::App::new(http::router(state()), guards());
    for entry in app.manifest() {
        if entry.method == Method::GET || entry.method == Method::HEAD {
            assert_eq!(
                entry.kind,
                RouteKind::ReadOnly,
                "{} {} is registered as mutating",
                entry.method,
                entry.path
            );
        }
    }
}

#[test]
fn every_registered_route_is_classified() {
    let app = http::App::new(http::router(state()), guards());
    assert!(
        !app.manifest().is_empty(),
        "the control listener registers no routes"
    );
    for entry in app.manifest() {
        let expected = if entry.method == Method::GET || entry.method == Method::HEAD {
            RouteKind::ReadOnly
        } else if entry.method == Method::POST
            || entry.method == Method::PUT
            || entry.method == Method::PATCH
            || entry.method == Method::DELETE
        {
            RouteKind::Mutating
        } else {
            panic!("{} is not a method this table records", entry.method);
        };
        assert_eq!(entry.kind, expected, "{} {}", entry.method, entry.path);
    }
}

#[test]
fn the_manifest_records_a_mutating_route() {
    // Without this, the two tests above could pass over a table that records nothing.
    let routes = Routes::new().post("/api/probe", || async { "ok" });
    let app = http::App::new(routes, guards());
    assert_eq!(app.manifest().len(), 1);
    assert_eq!(app.manifest()[0].method, Method::POST);
    assert_eq!(app.manifest()[0].kind, RouteKind::Mutating);
}

// ---- the guards are middleware a route cannot opt out of ------------------------------

#[tokio::test]
async fn guards_run_before_a_route_the_story_did_not_write() {
    // Registered here, not by `router()`: that is what "inherited by every later route" means.
    let routes = Routes::new().post("/api/probe", || async { "ok" });
    let served = Served::start(routes, GRACE).await;
    let refused = post(served.addr, "/api/probe", &[]).await;
    assert_eq!(refused.status, 403);
    assert_eq!(body_code(&refused), BAD_ORIGIN);
    let mut headers = vec![("Host", LOOPBACK)];
    headers.extend_from_slice(MUTATING_HEADERS);
    let accepted = send(served.addr, "POST", "/api/probe", &headers).await;
    assert_eq!(accepted.status, 200, "{}", accepted.body);
    assert_eq!(accepted.body, "ok");
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn guards_run_before_the_fallback() {
    let served = Served::start(http::router(state()), GRACE).await;
    let response = post(served.addr, "/api/nope", &[]).await;
    assert_eq!(response.status, 403, "the fallback answered instead");
    assert_eq!(body_code(&response), BAD_ORIGIN);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_rejected_request_is_still_counted() {
    // The counter runs before the checks, which is why a 403 appears in the log like any other
    // request (ADR-0010).
    let served = Served::start(http::router(state()), GRACE).await;
    let before = served.counter.served();
    post(served.addr, "/api/nope", &[]).await;
    post(served.addr, "/api/nope", &[]).await;
    let after = served.counter.served();
    assert_eq!(after - before, 2, "a refused request was not counted");
    served.finish().await.expect("a clean shutdown");
}

#[test]
fn the_layer_is_applied_once() {
    let source = include_str!("../src/http/app.rs");
    assert_eq!(
        source.matches(".layer(").count(),
        1,
        "ADR-0007's invariant is that `Router::layer` runs in exactly one place, `App::new`. \
         A second layer would answer before the counter runs (so a refused request would stop \
         being counted) or stop the guards being unavoidable — read ADR-0007 and ADR-0010 \
         before deleting this assertion."
    );
}
