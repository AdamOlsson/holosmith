//! The event log: one durable record per event, and the one writer that assigns its seq and
//! commits it (HS-009, ADR-0013).
//!
//! Nothing emits an event yet — HS-011 is the first producer. The handles exist now because the
//! rules they carry are the story: a producer appends over a bounded queue and gets backpressure;
//! a subscriber reads committed events only; and the writer assigns the seq, using the session's
//! `last_seq` counter, so a client never sees a seq a crash could undo.

pub mod registry;
pub mod repository;
pub(crate) mod stream;
mod writer;

use std::fmt;
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::sync::mpsc;

pub use registry::{StreamGuard, StreamRegistry};
pub use repository::EventRepository;
pub use writer::spawn;

/// One durable record in a session's stream: the row in `events`, with the seq the writer gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub session_id: String,
    /// Strictly increasing per session and never reused (ADR-0013).
    pub seq: i64,
    /// The `type` column: what happened. HS-011 names the kinds.
    pub kind: String,
    /// The `payload` column: JSON held as text, so the schema need not change with an event's
    /// shape.
    pub payload: String,
    /// Unix seconds, supplied by the producer rather than read by the writer, so a test asserts
    /// the value it wrote (ADR-0011's rule).
    pub created_at: i64,
}

impl Event {
    /// The durable record for `new`, once `seq` has been assigned. The repository calls it inside
    /// the insert's transaction, so the row and the returned `Event` cannot disagree.
    pub(crate) fn of(new: &NewEvent, seq: i64) -> Self {
        Self {
            session_id: new.session_id.clone(),
            seq,
            kind: new.kind.clone(),
            payload: new.payload.clone(),
            created_at: new.created_at,
        }
    }
}

/// What a producer appends: the event without its seq, which only the writer can assign.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    pub session_id: String,
    pub kind: String,
    pub payload: String,
    pub created_at: i64,
}

/// A producer's handle. Cheap to clone, and safe to hold in an `AppState`: it owns the senders,
/// not the writer thread.
#[derive(Debug, Clone)]
pub struct EventWriter {
    sender: mpsc::Sender<NewEvent>,
    /// The broadcast sender, kept so a handle can make a subscriber of its own.
    ///
    /// Without it the only `EventStream` would be the one `spawn` returned, and `serve` drops
    /// that: the broadcast half would be unreachable from the handle an `AppState` carries, and
    /// HS-010 would have to change `spawn`'s signature to subscribe at all.
    events: broadcast::Sender<Event>,
}

impl EventWriter {
    /// Queue one event for the writer.
    ///
    /// The queue is bounded, so this waits while it is full: a burst slows the producer and never
    /// the writer (ADR-0013). `Err` means the writer thread has stopped, which in a served process
    /// means the process is going down.
    pub async fn append(&self, event: NewEvent) -> Result<(), AppendError> {
        self.sender
            .send(event)
            .await
            .map_err(|_| AppendError::Stopped)
    }

    /// A subscriber for the events this writer commits from now on.
    ///
    /// Each call is an independent stream starting at the point of the broadcast: an event
    /// committed before the call is read back with its last-seen seq, not replayed (HS-010).
    pub fn subscribe(&self) -> EventStream {
        EventStream {
            receiver: self.events.subscribe(),
        }
    }
}

/// A subscriber's handle: committed events, in the order they were committed.
pub struct EventStream {
    receiver: broadcast::Receiver<Event>,
}

impl EventStream {
    /// The next committed event.
    ///
    /// `Err(RecvError::Lagged(n))` means this subscriber fell more than the buffer behind and
    /// missed `n` events; HS-010 re-fetches them by last-seen seq. `Err(RecvError::Closed)` means
    /// the writer has stopped.
    pub async fn recv(&mut self) -> Result<Event, broadcast::error::RecvError> {
        self.receiver.recv().await
    }
}

/// What the writer may vary: only how long a batch waits before it is committed.
#[derive(Debug, Clone, Copy)]
pub struct WriterOptions {
    /// The most an event waits between being appended and being broadcast. 25 ms in production;
    /// short in tests.
    pub commit_interval: Duration,
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            commit_interval: Duration::from_millis(25),
        }
    }
}

/// Why an append did not reach the writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendError {
    /// The writer thread has stopped, so the event will never be committed.
    Stopped,
}

impl fmt::Display for AppendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => f.write_str("the event writer has stopped; the event was not queued"),
        }
    }
}

impl std::error::Error for AppendError {}

/// Why a repository call failed.
#[derive(Debug)]
pub enum RepoError {
    /// SQLite refused a statement of the named call, the same convention `sessions::RepoError`
    /// follows.
    Sqlite {
        operation: &'static str,
        source: rusqlite::Error,
    },
    /// An event named a session that does not exist, so there is no counter to bump.
    UnknownSession(String),
    /// The counter bump lives in the sessions repository, and this is its failure.
    Sessions(crate::sessions::RepoError),
}

impl RepoError {
    /// A SQLite failure during `operation`: the name of the public method whose statement
    /// failed.
    fn sqlite(operation: &'static str, source: rusqlite::Error) -> Self {
        Self::Sqlite { operation, source }
    }
}

impl fmt::Display for RepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite { operation, source } => write!(f, "{operation}: {source}"),
            Self::UnknownSession(id) => {
                write!(f, "there is no session \"{id}\" to append an event to")
            }
            Self::Sessions(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for RepoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite { source, .. } => Some(source),
            Self::Sessions(error) => Some(error),
            Self::UnknownSession(_) => None,
        }
    }
}

/// A writer over a scratch database that lives as long as the returned directory, for a unit test
/// that has to hand a backend an `EventWriter` and does not need to read what it appended — the
/// stream those events reach is read off the wire instead (`tests/turns.rs`).
///
/// The directory is returned rather than dropped with the function, because a live connection must
/// not have its file unlinked underneath it (the rule `tests/support` documents for its own).
#[cfg(test)]
pub(crate) fn scratch_writer() -> (crate::sql::guard::TempDir, EventWriter) {
    // One directory per call: two tests of one binary run at the same time, and a shared scratch
    // directory would have one of them open a file the other is deleting.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let label = format!(
        "scratch-writer-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let dir = crate::sql::guard::TempDir::new(&label);
    let database = crate::db::Database::open(
        &dir.path().join(crate::db::DATABASE_FILE),
        crate::db::OpenOptions::default(),
        crate::clock::unix_now(),
    )
    .expect("a fresh database opens");
    let handle = spawn(
        std::sync::Arc::new(std::sync::Mutex::new(database)),
        WriterOptions::default(),
    )
    .0;
    (dir, handle)
}
