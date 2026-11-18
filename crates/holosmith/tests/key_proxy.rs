//! The two key-swapping proxies' listeners and their dummy-key check.
//!
//! The forwarding half is a real TLS connection to a provider or a tool's host, and is the verification; what this
//! file proves without the internet is the part that must never be wrong: a request with no valid dummy never
//! reaches a host, a known provider or tool with no stored key answers 503, and neither route parser can be told a
//! host.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use holosmith::backend::Backends;
use holosmith::container::ContainerSettings;
use holosmith::db::{Database, OpenOptions};
use holosmith::deleter::Deleter;
use holosmith::events::StreamRegistry;
use holosmith::key::{KEY_PROXY_ALIAS, KEY_PROXY_PORT, Store};
use holosmith::keyproxy;
use holosmith::mcpproxy;
use holosmith::runtime::Backend;
use holosmith::sessions::{Session, Status};
use holosmith::worktree::Retained;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("holosmith-keyproxy-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the temp directory is creatable");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn exchange(address: std::net::SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(address).expect("the proxy is listening");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("a read timeout");
    stream
        .write_all(request.as_bytes())
        .expect("the request is written");
    let mut response = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("the response is read: {error}"),
        }
    }
    String::from_utf8_lossy(&response).into_owned()
}

fn waiting(address: std::net::SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if TcpStream::connect(address).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the proxy did not bind {address}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_dummy_key_is_a_401_and_nothing_is_forwarded() {
    let directory = TempDir::new("no-key");
    let store = Arc::new(Store::new(directory.path.join("keys.toml")));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let address = listener.local_addr().expect("the bound address");
    let serve = tokio::spawn(keyproxy::serve(listener, Arc::clone(&store)));
    waiting(address);
    let response = exchange(
        address,
        "POST /anthropic/v1/messages HTTP/1.1\r\nHost: key-proxy:8080\r\nContent-Length: 2\r\n\r\n{}",
    );
    assert!(response.starts_with("HTTP/1.1 401 "), "{response}");
    assert!(response.contains("dummy key"), "{response}");
    serve.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_valid_dummy_with_no_stored_provider_key_is_a_503() {
    let directory = TempDir::new("no-provider");
    let store = Arc::new(Store::new(directory.path.join("keys.toml")));
    let dummy = store.dummy_for("0123456789abcdef").expect("a dummy");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let address = listener.local_addr().expect("the bound address");
    let serve = tokio::spawn(keyproxy::serve(listener, Arc::clone(&store)));
    waiting(address);
    let response = exchange(
        address,
        &format!(
            "POST /anthropic/v1/messages HTTP/1.1\r\nHost: key-proxy:8080\r\nx-api-key: {dummy}\r\nContent-Length: 2\r\n\r\n{{}}"
        ),
    );
    assert!(response.starts_with("HTTP/1.1 503 "), "{response}");
    assert!(response.contains("key set anthropic"), "{response}");
    serve.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn either_shape_of_a_provider_without_a_key_names_that_provider() {
    // `opencode-go` serves two APIs under one host, and the row selects one of them from the endpoint pi
    // appended. Neither selection changes which provider's key is looked for: a machine with no `opencode-go` key
    // answers both with the one sentence that names `key set`, before any TLS client is started.
    let directory = TempDir::new("no-provider-shapes");
    let store = Arc::new(Store::new(directory.path.join("keys.toml")));
    let dummy = store.dummy_for("0123456789abcdef").expect("a dummy");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let address = listener.local_addr().expect("the bound address");
    let serve = tokio::spawn(keyproxy::serve(listener, Arc::clone(&store)));
    waiting(address);
    for target in ["/opencode-go/v1/messages", "/opencode-go/chat/completions"] {
        let response = exchange(
            address,
            &format!(
                "POST {target} HTTP/1.1\r\nHost: key-proxy:8080\r\nx-api-key: {dummy}\r\nContent-Length: 2\r\n\r\n{{}}"
            ),
        );
        assert!(
            response.starts_with("HTTP/1.1 503 "),
            "{target}: {response}"
        );
        assert!(
            response.contains("key set opencode-go"),
            "{target}: {response}"
        );
    }
    serve.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_delete_flow_revokes_the_sessions_dummy_key() {
    let directory = TempDir::new("revoke");
    let store = Store::new(directory.path.join("secrets/keys.toml"));
    let dummy = store.dummy_for("0123456789abcdef").expect("a dummy");
    assert!(Store::session_of(&store.load().expect("the store reads"), &dummy).is_some());
    let database = Database::open(
        &directory.path.join("app.db"),
        OpenOptions::default(),
        1_760_000_000,
    )
    .expect("the test database opens");
    let session = Session {
        id: "0123456789abcdef".to_string(),
        project: "holosmith".to_string(),
        model: "claude-sonnet-4.5".to_string(),
        thinking: "high".to_string(),
        worktree: String::new(),
        base_commit: None,
        status: Status::Deleting,
        status_reason: None,
        client_key: None,
        network: false,
        created_at: 1_760_000_000,
        last_active_at: 1_760_000_000,
        stopped_at: None,
        usage_in: 0,
        usage_out: 0,
    };
    let deleter = Deleter::new(
        Backends::for_run(
            Backend::Stub,
            ContainerSettings::at(directory.path.join("projects"), directory.path.join("data")),
        ),
        Arc::new(std::sync::Mutex::new(database)),
        StreamRegistry::new(),
        store.path().to_path_buf(),
    );
    deleter.remove(&session, &Retained::nothing()).await;
    let secrets = store.load().expect("the store reads after the delete");
    assert!(
        Store::session_of(&secrets, &dummy).is_none(),
        "the deleted session's dummy is still usable: {secrets:?}"
    );
}

#[test]
fn the_names_are_the_alias_and_the_port_the_models_file_uses() {
    assert_eq!(KEY_PROXY_ALIAS, "key-proxy");
    assert_eq!(KEY_PROXY_PORT, 8080);
    assert_eq!(mcpproxy::MCP_PROXY_ALIAS, "mcp-proxy");
    assert_eq!(mcpproxy::MCP_PROXY_PORT, 8090);
    assert_ne!(
        mcpproxy::MCP_PROXY_ALIAS,
        KEY_PROXY_ALIAS,
        "one alias for two containers would be one route to two different keys"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mcp_proxy_answers_a_request_with_no_dummy_before_it_is_forwarded() {
    // An MCP request is authenticated by the session's dummy, exactly as a model call is: the session network is the
    // first half of "only sessions", and this is the second. Nothing is forwarded, so no key is spent and no host is
    // contacted.
    let directory = TempDir::new("mcp-no-key");
    let store = Arc::new(Store::new(directory.path.join("keys.toml")));
    store
        .set_tool("linear", "lin_api_the_tool_key")
        .expect("a tool key is storable");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let address = listener.local_addr().expect("the bound address");
    let serve = tokio::spawn(mcpproxy::serve(listener, Arc::clone(&store)));
    waiting(address);
    let response = exchange(
        address,
        "POST /linear/mcp HTTP/1.1\r\nHost: mcp-proxy:8090\r\nAccept: application/json, text/event-stream\r\n\
         Content-Length: 2\r\n\r\n{}",
    );
    assert!(response.starts_with("HTTP/1.1 401 "), "{response}");
    assert!(response.contains("dummy key"), "{response}");
    serve.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_valid_dummy_with_no_stored_tool_key_names_the_command_that_stores_one() {
    let directory = TempDir::new("mcp-no-tool");
    let store = Arc::new(Store::new(directory.path.join("keys.toml")));
    let dummy = store.dummy_for("0123456789abcdef").expect("a dummy");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let address = listener.local_addr().expect("the bound address");
    let serve = tokio::spawn(mcpproxy::serve(listener, Arc::clone(&store)));
    waiting(address);
    for target in ["/linear/mcp", "/linear/mcp/readonly"] {
        let response = exchange(
            address,
            &format!(
                "POST {target} HTTP/1.1\r\nHost: mcp-proxy:8090\r\nAuthorization: Bearer {dummy}\r\n\
                 Content-Length: 2\r\n\r\n{{}}"
            ),
        );
        assert!(
            response.starts_with("HTTP/1.1 503 "),
            "{target}: {response}"
        );
        assert!(response.contains("key set linear"), "{target}: {response}");
    }
    serve.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dummy_is_checked_before_the_tool_is_and_the_route_comes_from_the_table() {
    // A route this build does not hold a key for is refused 403 without the store being asked, and a bad dummy on a
    // route it does hold is 401: the two refusals are different facts and an operator reads them as such.
    let directory = TempDir::new("mcp-route");
    let store = Arc::new(Store::new(directory.path.join("keys.toml")));
    let dummy = store.dummy_for("0123456789abcdef").expect("a dummy");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let address = listener.local_addr().expect("the bound address");
    let serve = tokio::spawn(mcpproxy::serve(listener, Arc::clone(&store)));
    waiting(address);
    let unknown_tool = exchange(
        address,
        "POST /not-a-tool/mcp HTTP/1.1\r\nHost: mcp-proxy:8090\r\n\r\n",
    );
    assert!(unknown_tool.starts_with("HTTP/1.1 403 "), "{unknown_tool}");
    assert!(
        unknown_tool.contains("linear"),
        "the refusal does not name the tools this build knows: {unknown_tool}"
    );
    let unknown_endpoint = exchange(
        address,
        "POST /linear/sse HTTP/1.1\r\nHost: mcp-proxy:8090\r\n\r\n",
    );
    assert!(
        unknown_endpoint.starts_with("HTTP/1.1 403 "),
        "{unknown_endpoint}"
    );
    assert!(
        unknown_endpoint.contains("/mcp/readonly"),
        "the refusal does not name the endpoints the row serves: {unknown_endpoint}"
    );
    let bad_dummy = exchange(
        address,
        "POST /linear/mcp HTTP/1.1\r\nHost: mcp-proxy:8090\r\nAuthorization: Bearer hs-dummy-not-a-session\r\n\r\n",
    );
    assert!(bad_dummy.starts_with("HTTP/1.1 401 "), "{bad_dummy}");
    assert!(
        bad_dummy.contains("not valid for any session"),
        "{bad_dummy}"
    );
    // And the dummy this machine minted is the one that gets past the check, which is what makes the two refusals
    // above about the route rather than about a machine with no sessions.
    let known = exchange(
        address,
        &format!(
            "POST /linear/mcp HTTP/1.1\r\nHost: mcp-proxy:8090\r\nAuthorization: Bearer {dummy}\r\n\
             Content-Length: 2\r\n\r\n{{}}"
        ),
    );
    assert!(known.starts_with("HTTP/1.1 503 "), "{known}");
    serve.abort();
}
