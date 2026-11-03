#![allow(dead_code)]
//! The harness every socket-level test shares: a real `App` on an ephemeral port, raw HTTP/1.1
//! over a socket, and the parsing a test needs to read the answer.
//!
//! The module is compiled into each test binary separately, so the helpers one binary does not
//! use are dead there; the `allow` above is what keeps `clippy -D warnings` a gate.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use holosmith::backend::{Backends, StubBackend};
use holosmith::clock;
use holosmith::config::Config;
use holosmith::db::{self, Database, OpenOptions};
use holosmith::events::{self, EventWriter, StreamRegistry, WriterOptions};
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
    static SHARED: OnceLock<(TempDir, Arc<Mutex<Database>>, EventWriter)> = OnceLock::new();
    let (_, database, events) = SHARED.get_or_init(|| {
        let dir = TempDir::new("shared");
        let database = Arc::new(Mutex::new(open(&dir)));
        let events = writer_handle(Arc::clone(&database));
        (dir, database, events)
    });
    AppState {
        config: Config::default(),
        runtime: Runtime::started(Backend::Stub),
        database: Arc::clone(database),
        events: events.clone(),
        streams: StreamRegistry::default(),
        backend: Backends::Stub(slow_stub()),
    }
}

/// How long the harness's stub backend takes per provisioning step.
///
/// Long enough that a test which creates a session and immediately reads its row cannot lose the
/// race against the provisioner that HS-011 now starts in the background: a session stays
/// `starting` for a minute unless a test asks for a faster stub with [`with_stub`]. Nothing waits
/// that minute — the provisioner is a spawned task and the test's runtime drops it at the end.
pub const SLOW_STUB_STEP: Duration = Duration::from_secs(60);

/// The harness's default stub: one that does not finish while a test is looking.
pub fn slow_stub() -> StubBackend {
    StubBackend::new(SLOW_STUB_STEP)
}

/// A stub that finishes a step in a millisecond, for a test that wants provisioning to *complete*
/// while it waits: the four steps are then four separate commits and the session reaches `idle`.
pub fn fast_stub() -> StubBackend {
    StubBackend::new(Duration::from_millis(1))
}

/// The same state with the stub backend `stub` replaces, so a test can pace or fail provisioning
/// without the backend being decided by a config file inside the handler.
pub fn with_stub(mut state: AppState, stub: StubBackend) -> AppState {
    state.backend = Backends::Stub(stub);
    state
}

/// A state for a test that writes: its own database in its own directory, so no two tests (and
/// no two `Database::open` calls in one process) share a file.
pub fn state_in(dir: &TempDir, config: Config) -> AppState {
    state_with_writer(dir, config, WriterOptions::default())
}

/// A state whose writer commits on `options.commit_interval`, so a test can hold an event
/// uncommitted and choose which half of a stream's replay window it lands in.
pub fn state_with_writer(dir: &TempDir, config: Config, options: WriterOptions) -> AppState {
    let database = Arc::new(Mutex::new(open(dir)));
    AppState {
        config,
        runtime: Runtime::started(Backend::Stub),
        events: writer_handle_with(Arc::clone(&database), options),
        database,
        streams: StreamRegistry::default(),
        backend: Backends::Stub(slow_stub()),
    }
}

/// The writer handle for a state's database. The tests that use it emit no events, so the
/// default interval never runs; the thread exits when the state's last handle drops.
fn writer_handle(database: Arc<Mutex<Database>>) -> EventWriter {
    writer_handle_with(database, WriterOptions::default())
}

fn writer_handle_with(database: Arc<Mutex<Database>>, options: WriterOptions) -> EventWriter {
    events::spawn(database, options).0
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

/// One SSE block: the three fields the client uses.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// The SSE `id`, which is the seq.
    pub id: Option<String>,
    /// The SSE `event`, which is the stored kind.
    pub kind: Option<String>,
    pub data: String,
}

impl Frame {
    pub fn parse(block: &str) -> Self {
        let mut frame = Self::default();
        for line in block.lines() {
            // A comment line — axum's keep-alive — has no field name, so it parses to nothing.
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.strip_prefix(' ').unwrap_or(value);
            match name {
                "id" => frame.id = Some(value.to_string()),
                "event" => frame.kind = Some(value.to_string()),
                "data" => {
                    if !frame.data.is_empty() {
                        frame.data.push('\n');
                    }
                    frame.data.push_str(value);
                }
                _ => {}
            }
        }
        frame
    }

    pub fn seq(&self) -> i64 {
        self.id
            .as_deref()
            .expect("the frame carries an id")
            .parse()
            .expect("the id is the seq")
    }
}

/// An open stream: the answer's head, then one block at a time.
pub struct Sse {
    socket: TcpStream,
    pending: Vec<u8>,
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

impl Sse {
    /// Open a stream for `session`, resuming after `last_event_id` when one is given.
    pub async fn connect(addr: SocketAddr, session: &str, last_event_id: Option<i64>) -> Self {
        let mut socket = TcpStream::connect(addr).await.expect("the server accepts");
        let mut request = format!(
            "GET /api/sessions/{session}/events HTTP/1.0\r\nHost: 127.0.0.1\r\n\
             Connection: close\r\n"
        );
        if let Some(seq) = last_event_id {
            request.push_str(&format!("Last-Event-ID: {seq}\r\n"));
        }
        request.push_str("\r\n");
        socket
            .write_all(request.as_bytes())
            .await
            .expect("the request is written");

        let mut head = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            if let Some(end) = head.windows(4).position(|window| window == b"\r\n\r\n") {
                let body = head.split_off(end + 4);
                let answer = parse(&head);
                return Self {
                    socket,
                    pending: body,
                    status: answer.status,
                    headers: answer.headers,
                };
            }
            let read = tokio::time::timeout(PATIENCE, socket.read(&mut chunk))
                .await
                .expect("the answer starts within the deadline")
                .expect("the answer is readable");
            assert!(read > 0, "the server closed before answering");
            head.extend_from_slice(&chunk[..read]);
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The next block, or `None` once the server ends the stream.
    pub async fn next_block(&mut self, bound: Duration) -> Option<Frame> {
        let deadline = Instant::now() + bound;
        loop {
            if let Some(end) = self.pending.windows(2).position(|window| window == b"\n\n") {
                let block: Vec<u8> = self.pending.drain(..end + 2).collect();
                let text = String::from_utf8(block[..block.len() - 2].to_vec())
                    .expect("an SSE block is UTF-8");
                return Some(Frame::parse(&text));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "no SSE block arrived within {bound:?}; pending: {:?}",
                String::from_utf8_lossy(&self.pending)
            );
            let mut chunk = [0u8; 4096];
            match tokio::time::timeout(remaining, self.socket.read(&mut chunk)).await {
                Err(_) => panic!("no SSE block arrived within {bound:?}"),
                Ok(Ok(0)) => {
                    assert!(
                        self.pending.is_empty(),
                        "the stream ended mid-block: {:?}",
                        String::from_utf8_lossy(&self.pending)
                    );
                    return None;
                }
                Ok(Ok(read)) => self.pending.extend_from_slice(&chunk[..read]),
                Ok(Err(error)) => panic!("the stream could not be read: {error}"),
            }
        }
    }

    /// The next `count` blocks, failing if the stream ends first.
    pub async fn take(&mut self, count: usize, bound: Duration) -> Vec<Frame> {
        let mut frames = Vec::with_capacity(count);
        for _ in 0..count {
            frames.push(
                self.next_block(bound)
                    .await
                    .expect("the stream ended before the events ran out"),
            );
        }
        frames
    }

    /// Every remaining block, which means reading until the stream ends.
    pub async fn drain(&mut self, bound: Duration) -> Vec<Frame> {
        let mut frames = Vec::new();
        while let Some(frame) = self.next_block(bound).await {
            frames.push(frame);
        }
        frames
    }

    /// Whether the server wrote nothing at all within `bound`.
    pub async fn quiet_for(&mut self, bound: Duration) -> bool {
        assert!(
            self.pending.is_empty(),
            "a block was already waiting: {:?}",
            String::from_utf8_lossy(&self.pending)
        );
        let mut chunk = [0u8; 64];
        match tokio::time::timeout(bound, self.socket.read(&mut chunk)).await {
            Err(_) => true,
            Ok(Ok(read)) => {
                self.pending.extend_from_slice(&chunk[..read]);
                false
            }
            Ok(Err(_)) => false,
        }
    }
}

/// How long a test waits for something the server does before calling it stuck. One value for
/// every socket-level test, so no two of them disagree about what "slow" means.
pub const PATIENCE: Duration = Duration::from_secs(10);
