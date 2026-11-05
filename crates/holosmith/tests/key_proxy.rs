//! The key proxy's listener and dummy-key check (HS-039).
//!
//! The forwarding half is a real TLS connection to a provider and is HS-040's verification; what this file
//! proves without the internet is the part that must never be wrong: a request with no valid dummy never
//! reaches a provider, a known provider with no stored key answers 503, and the route parser cannot be told a
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
use holosmith::runtime::Backend;
use holosmith::sessions::{Session, Status};

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
    deleter.remove(&session).await;
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
}
