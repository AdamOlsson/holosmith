//! The assembled service as a client sees it, and the two ways it stops.
//!
//! Every test starts the real `App` on an ephemeral port and talks HTTP/1.1 over a socket, so
//! the status, the headers and the bytes are the ones a browser gets. No client crate: the
//! requests are tiny and one of them has to be slow or hanging.

use std::time::{Duration, Instant};

use holosmith::config::Config;
use holosmith::http::{self, Routes};
use holosmith::web;

mod support;
use support::{Served, TempDir, fetch, fetch_head, state, state_in, version};

/// RFC 8291 §5's application-server point, as base64url: the one P-256 public key this suite has a published
/// vector for, so "the answer carries the pair's public half" can be asserted byte for byte.
const AS_PUBLIC: &str =
    "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";

#[tokio::test]
async fn the_health_endpoint_answers_the_seven_keys() {
    // The data root is this test's own: `Config::default` names the *machine's*, and a machine that has deployed
    // holds a pair — which would make the sixth key a fact about the host rather than about this test (,
    // the same hazard `health.rs`'s unit test works around).
    let dir = TempDir::new("health-six-keys");
    let config = Config {
        data_root: dir.path().join("data"),
        ..Config::default()
    };
    let served = Served::start(
        http::router(state_in(&dir, config.clone())),
        Duration::from_secs(10),
    )
    .await;
    let response = fetch(served.addr, "/api/health").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let expected_prefix = format!(
        r#"{{"name":"holosmith","version":"{}","role":"both","uptime_s":"#,
        version()
    );
    assert!(
        response.body.starts_with(&expected_prefix),
        "{}",
        response.body
    );
    assert!(
        response
            .body
            .contains(r#""preview_origin":"http://localhost:8081""#),
        "the fifth key is the preview origin: {}",
        response.body
    );
    // The sixth is `null` on a machine with no pair, and the fifth keeps its place: the keys before it are
    // the order a client reads.
    assert!(
        response.body.contains(r#","application_server_key":null,"#),
        "a machine with no pair answers an explicit null: {}",
        response.body
    );
    // The seventh is what this machine calls itself in a notification's payload, and a machine with no name
    // of its own is `local` — the reserved word for *the machine whose page this is*.
    assert!(
        response.body.ends_with(r#","machine_name":"local"}"#),
        "the seventh key is this machine's own name: {}",
        response.body
    );

    // And with a pair on disk it is that pair's **public** half, byte for byte — not a re-derivation, which is
    // what makes "each machine's keys are independent" a fact a browser can rely on.
    let path = holosmith::vapid::store(&config.data_root);
    std::fs::create_dir_all(path.parent().expect("the secrets directory")).expect("the directory");
    let pair = holosmith::vapid::Pair {
        public_key: AS_PUBLIC.to_string(),
        private_key: AS_PRIVATE.to_string(),
    };
    std::fs::write(&path, holosmith::vapid::render(&pair)).expect("the pair is writable");
    let response = fetch(served.addr, "/api/health").await;
    assert_eq!(
        response.body,
        format!(
            r#"{{"name":"holosmith","version":"{}","role":"both","uptime_s":{},"preview_origin":"http://localhost:8081","application_server_key":"{AS_PUBLIC}","machine_name":"local"}}"#,
            version(),
            // The uptime is the one field that moves; the assertion is the whole shape around it.
            response
                .body
                .split(r#""uptime_s":"#)
                .nth(1)
                .and_then(|rest| rest.split(',').next())
                .expect("the uptime is in the payload")
        ),
        "the sixth key is the pair's public half and the private half is nowhere: {}",
        response.body
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn head_on_the_health_endpoint_returns_the_headers_and_no_body() {
    let served = Served::start(http::router(state()), Duration::from_secs(10)).await;
    let get = fetch(served.addr, "/api/health").await;
    let head = fetch_head(served.addr, "/api/health").await;
    assert_eq!(head.status, 200);
    assert_eq!(head.body, "");
    assert_eq!(head.header("content-type"), get.header("content-type"));
    assert_eq!(head.header("content-length"), get.header("content-length"));
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_app_shell_is_served_at_the_root() {
    let served = Served::start(http::router(state()), Duration::from_secs(10)).await;
    let response = fetch(served.addr, "/").await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(response.body, web::index_html());
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_shipped_asset_is_served_with_its_content_type() {
    let served = Served::start(http::router(state()), Duration::from_secs(10)).await;
    if web::is_placeholder() {
        eprintln!("SKIP: this build carries the placeholder, so the app ships no assets");
        served.finish().await.expect("a clean shutdown");
        return;
    }
    let (bytes, content_type) = web::asset("/favicon.svg").expect("a built app ships a favicon");
    let response = fetch(served.addr, "/favicon.svg").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some(content_type));
    assert_eq!(response.body, String::from_utf8_lossy(bytes));
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_path_outside_api_is_the_app_shell() {
    let served = Served::start(http::router(state()), Duration::from_secs(10)).await;
    let response = fetch(served.addr, "/sessions/7").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, web::index_html());
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn an_unknown_path_under_assets_is_the_app_shell() {
    // Recorded decision: only `/api/` gets ADR-0008's envelope, so an asset the app does not
    // ship is the shell. Pinned here so a later story that serves 404 for a missing asset has
    // to change a test rather than surprise a browser.
    let served = Served::start(http::router(state()), Duration::from_secs(10)).await;
    let response = fetch(served.addr, "/assets/absent.js").await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(response.body, web::index_html());
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn an_unknown_path_under_api_is_the_json_envelope() {
    let served = Served::start(http::router(state()), Duration::from_secs(10)).await;
    let response = fetch(served.addr, "/api/nope").await;
    assert_eq!(response.status, 404);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert_eq!(
        response.body,
        r#"{"code":"not_found","message":"no such endpoint"}"#
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_edge_layer_runs_on_routes_and_on_the_fallback() {
    let served = Served::start(http::router(state()), Duration::from_secs(10)).await;
    let before = served.counter.served();
    fetch(served.addr, "/api/health").await;
    fetch(served.addr, "/").await;
    fetch(served.addr, "/neither-a-route-nor-an-asset").await;
    fetch(served.addr, "/api/nope").await;
    let after = served.counter.served();
    assert_eq!(after - before, 4, "the layer missed a path");
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn an_idle_server_stops_at_once() {
    let served = Served::start(http::router(state()), Duration::from_secs(10)).await;
    let started = Instant::now();
    served.finish().await.expect("a clean shutdown");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "an idle server took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn an_in_flight_request_finishes_before_the_process_stops() {
    let routes = Routes::new().get("/slow", || async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        "finished"
    });
    let mut served = Served::start(routes, Duration::from_secs(10)).await;
    let addr = served.addr;
    let in_flight = tokio::spawn(async move { fetch(addr, "/slow").await });
    // Let the request reach the handler before the signal.
    tokio::time::sleep(Duration::from_millis(100)).await;
    served.stop();
    let response = in_flight.await.expect("the request task does not panic");
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "finished");
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_hanging_request_is_cut_at_the_deadline() {
    let routes = Routes::new().get("/hang", || async {
        std::future::pending::<()>().await;
        "never"
    });
    let mut served = Served::start(routes, Duration::from_millis(200)).await;
    let addr = served.addr;
    let hung = tokio::spawn(async move { fetch(addr, "/hang").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = Instant::now();
    served.stop();
    let stopped = tokio::time::timeout(Duration::from_secs(5), served.join())
        .await
        .expect("the drain deadline ends the wait");
    stopped.expect("a drain deadline is still a clean stop");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the deadline was not honoured: {:?}",
        started.elapsed()
    );
    hung.abort();
}
