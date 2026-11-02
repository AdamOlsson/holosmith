//! The assembled service as a client sees it, and the two ways it stops (HS-003).
//!
//! Every test starts the real `App` on an ephemeral port and talks HTTP/1.1 over a socket, so
//! the status, the headers and the bytes are the ones a browser gets. No client crate: the
//! requests are tiny and one of them has to be slow or hanging.

use std::time::{Duration, Instant};

use holosmith::http::{self, Routes};
use holosmith::web;

mod support;
use support::{Served, fetch, fetch_head, state, version};

#[tokio::test]
async fn the_health_endpoint_answers_the_four_keys() {
    let served = Served::start(http::router(state()), Duration::from_secs(10)).await;
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
    assert!(response.body.ends_with('}'), "{}", response.body);
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
