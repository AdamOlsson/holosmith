#![allow(dead_code)]
//! The harness every socket-level test shares: a real `App` on an ephemeral port, raw HTTP/1.1
//! over a socket, and the parsing a test needs to read the answer.
//!
//! The module is compiled into each test binary separately, so the helpers one binary does not
//! use are dead there; the `allow` above is what keeps `clippy -D warnings` a gate.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use holosmith::clock;
use holosmith::config::Config;
use holosmith::db::{self, Database, OpenOptions};
use holosmith::http::{App, AppState, Guards, RequestCounter, Routes};
use holosmith::runtime::{Backend, Runtime};
use holosmith::serve::{self, ServeOptions};
use holosmith::version::VersionInfo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

/// A directory that removes itself, so the tests leave nothing in `/tmp`.
///
/// The one copy every socket-level and database test shares (PER-39's third copy). The label
/// names the test and the process id is what makes two concurrent runs safe: without it, two
/// `cargo test` invocations — or two binaries that use the same label — would `remove_dir_all`
/// each other's live directories and open database. Nothing asserts the prefix, and `new` clears
/// a directory before using it, so a name it reuses is never stale.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("holosmith-test-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the temp directory is creatable");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    pub fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.file(name);
        std::fs::write(&path, contents).expect("the temp file is writable");
        path
    }

    pub fn database(&self) -> PathBuf {
        self.file(db::DATABASE_FILE)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Open (and migrate) `dir`'s database, the one opener the tests share.
pub fn open(dir: &TempDir) -> Database {
    Database::open(&dir.database(), OpenOptions::default(), clock::unix_now())
        .expect("a fresh database opens")
}

/// A state for the control listener's own route table: the default config, the stub backend and
/// one process-wide database.
///
/// The database is opened exactly once because two `Database::open` calls on one path in one
/// process is ADR-0009's measured "database is locked" trap. Its directory is held for the
/// process lifetime, because a live connection must not have its file unlinked, and its name
/// carries the process id, so two concurrent runs never share it. Tests that use this state
/// never write.
pub fn state() -> AppState {
    static SHARED: OnceLock<(TempDir, Arc<Mutex<Database>>)> = OnceLock::new();
    let (_, database) = SHARED.get_or_init(|| {
        let dir = TempDir::new("shared");
        let database = Arc::new(Mutex::new(open(&dir)));
        (dir, database)
    });
    AppState {
        config: Config::default(),
        runtime: Runtime::started(Backend::Stub),
        database: Arc::clone(database),
    }
}

/// A state for a test that writes: its own database in its own directory, so no two tests (and
/// no two `Database::open` calls in one process) share a file.
pub fn state_in(dir: &TempDir, config: Config) -> AppState {
    AppState {
        config,
        runtime: Runtime::started(Backend::Stub),
        database: Arc::new(Mutex::new(open(dir))),
    }
}

/// The version this build reports, for a payload that names it.
pub fn version() -> String {
    VersionInfo::current().version
}

/// A running app, the handle that stops it, and where it listens.
pub struct Served {
    pub addr: SocketAddr,
    pub counter: RequestCounter,
    stop: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<Result<(), String>>,
}

impl Served {
    /// Serve `routes` with the guards the default config produces.
    pub async fn start(routes: Routes, grace: Duration) -> Self {
        Self::start_with(routes, Guards::from_config(&Config::default()), grace).await
    }

    /// Serve `routes` behind `guards`, so a test can choose what the machine answers.
    pub async fn start_with(routes: Routes, guards: Guards, grace: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral port is bindable");
        let addr = listener.local_addr().expect("a bound address");
        let (stop, stopped) = oneshot::channel();
        let app = App::new(routes, guards);
        let counter = app.counter();
        let handle = tokio::spawn(async move {
            serve::run(
                listener,
                app,
                ServeOptions {
                    shutdown_grace: grace,
                },
                async move {
                    let _ = stopped.await;
                },
            )
            .await
            .map_err(|error| error.to_string())
        });
        Self {
            addr,
            counter,
            stop: Some(stop),
            handle,
        }
    }

    /// Ask the app to stop; the drain still runs.
    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    /// Stop the app and wait for the serving task to finish.
    pub async fn finish(mut self) -> Result<(), String> {
        self.stop();
        self.join().await
    }

    /// Wait for the serving task, so a test can put its own deadline around the drain.
    pub async fn join(self) -> Result<(), String> {
        self.handle.await.expect("the serving task does not panic")
    }
}

/// An answer as a test reads it: the status, every header, and the body as text.
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// One request/response over a fresh connection, `Connection: close` so the read ends.
pub async fn request(addr: SocketAddr, request: &str) -> Response {
    let mut stream = TcpStream::connect(addr).await.expect("the server accepts");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("the request is written");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("the response is read");
    parse(&raw)
}

/// `get`/`head` for a path, with the host header a browser would send.
pub async fn fetch(addr: SocketAddr, path: &str) -> Response {
    request(
        addr,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
    .await
}

pub async fn fetch_head(addr: SocketAddr, path: &str) -> Response {
    request(
        addr,
        &format!("HEAD {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
    .await
}

pub fn parse(raw: &[u8]) -> Response {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a response has a header block");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let body = String::from_utf8_lossy(&raw[split + 4..]).to_string();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("a status line starts with the code");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect();
    Response {
        status,
        headers,
        body,
    }
}
