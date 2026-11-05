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
pub use repository::{Compaction, EventRepository, Truncation};
pub use writer::spawn;

// The kinds whose payload ADR-0018 and ADR-0019 pin, and the only spelling of each.
//
// They live here rather than next to their producer because two modules now name one of them: the stub
// emits every kind, and the writer reads a `message.end`'s `usage` to keep the session's tally (HS-023).
// A kind its own producer alone names would not need a constant; these do, and a second literal is how
// the renderer and the producer drift apart. `session.status` is not here: the transitioner is its only
// producer and `sessions::state::SESSION_STATUS` is its name.

/// A turn beginning: `{}`.
pub const TURN_START: &str = "turn.start";
/// One appended piece of a message: `{"message_id", "kind": "text"|"thinking", "text"}`.
pub const MESSAGE_DELTA: &str = "message.delta";
/// A message's whole state, the only replacing frame: `{"message_id", "text", "thinking"}`.
pub const MESSAGE_SNAPSHOT: &str = "message.snapshot";
/// A tool call beginning: `{"tool_id", "name", "arguments"}`.
pub const TOOL_START: &str = "tool.start";
/// A tool call finishing: `{"tool_id", "name", "output"}`.
pub const TOOL_END: &str = "tool.end";
/// A message finishing: `{"message_id", "usage": {"in", "out"}, "stop_reason", "tool_calls"}`. The
/// `usage` is what the writer adds to the session's tally, and it is optional: a backend that cannot
/// report it — **or that reports a sum of zero**, which is the same thing said twice — emits the frame
/// without it (HS-023, ADR-0042). Its two numbers are the message's *cost*: `in` is the prompt including
/// whatever the provider served from its cache, which is why pi's `cacheRead`/`cacheWrite` are folded into
/// it (ADR-0042). `stop_reason` and `tool_calls` (the message's
/// tool-call names, in order) are additive keys HS-032 added for pi's answers — ADR-0040, amending
/// ADR-0019's table, and nothing reads them yet: the arguments and output of a call are
/// `tool.start`/`tool.end` (HS-033).
pub const MESSAGE_END: &str = "message.end";
/// A turn ending: `{}`.
pub const TURN_END: &str = "turn.end";
/// The operator's guidance reaching a running turn: `{"text"}`, and the one kind with no message id
/// (ADR-0021).
pub const TURN_STEER: &str = "turn.steer";
/// Something a turn could not do, said out loud: `{"reason", "method"}` (HS-035, ADR-0043).
///
/// Its one producer today is an extension's dialog, which a session cannot answer and therefore cancels; the
/// frame is why an extension's behaviour is explicable instead of mysterious, and it has no message id for the
/// reason a steer has none — one frame, one block, nothing continues it. `reason` is the sentence a screen
/// shows and `method` is pi's own name for what was asked.
pub const TURN_ERROR: &str = "turn.error";

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

/// The pinned payload of one delta, as the writer's coalescer and the compactor's fold read it
/// (HS-059, HS-060).
///
/// Field order is the contract's: serialising this spells a merged row exactly like the one a producer
/// wrote (`{"message_id":…,"kind":…,"text":…}`, ADR-0018), so a replay cannot tell which rows were merged.
///
/// One parser, two readers: the writer merges a batch with it and the compactor folds a history with it.
/// A second reading of "what a delta is" is how the two would drift apart about a payload neither
/// producer wrote. Both readers live in `events`, so the struct stays private to the module — a child
/// module sees it already, and `pub(super)` here would mean the crate root.
#[derive(Debug, serde::Serialize)]
struct Delta {
    message_id: String,
    kind: String,
    text: String,
}

/// Read `payload` as a delta, or `None` when it is not exactly the pinned shape.
///
/// Exactly: the three keys and nothing else. A producer that adds a fourth key has invented a payload the
/// contract does not have, and merging it would drop that key on the floor — the writer is not a producer's
/// validator and must not silently rewrite what it does not understand (ADR-0031).
fn parse_delta(payload: &str) -> Option<Delta> {
    let event: serde_json::Value = serde_json::from_str(payload).ok()?;
    let object = event.as_object()?;
    if object.len() != 3 {
        return None;
    }
    Some(Delta {
        message_id: event.get("message_id")?.as_str()?.to_string(),
        kind: event.get("kind")?.as_str()?.to_string(),
        text: event.get("text")?.as_str()?.to_string(),
    })
}

/// The run still open at the end of the batch: which row it is, what it has accumulated, and how many
/// deltas went into it (a run of one is never re-serialised, so a lone delta keeps its producer's bytes).
struct Open {
    row: usize,
    delta: Delta,
    deltas: usize,
}

/// Merge the consecutive deltas of one message and kind inside one commit's batch (HS-059).
///
/// Spec item 15: "inside each 25 ms commit, consecutive deltas of the same kind and message become one row
/// with one seq". A streaming agent emits far more deltas than the log should hold, and the transcript
/// cannot tell the difference: a delta is one appended text node, so appending one row's text and appending
/// ten rows' texts in order are the same rendering (ADR-0018).
///
/// Four things must all match, and each one is a boundary the story names: the **session** (one batch can
/// carry two sessions' events), the **kind** (`message.delta` and nothing else), the **message id** (a
/// message's block is keyed by it), and the delta's own **kind** (`text` or `thinking`, which are separate
/// nodes in one block). Anything between two deltas — a tool call, a `message.end` — ends the run: the
/// rule is *consecutive* deltas, and a batch that interleaves them has an order the transcript depends on.
///
/// The merged row keeps the **first** event's `created_at`: the row stands for a span, and the span began
/// there. Its payload is re-serialised **once, when the run ends** — the open run is accumulated outside the
/// rows, so a batch of *n* deltas costs *n* appends and one serialise rather than *n* re-writes of the whole
/// text (the argument ADR-0018 makes against re-reading a message to append a delta). An event whose payload
/// is not the pinned shape is passed through untouched and ends any run it is part of.
pub(crate) fn coalesce(batch: Vec<NewEvent>) -> Vec<NewEvent> {
    let mut merged: Vec<NewEvent> = Vec::with_capacity(batch.len());
    let mut open: Option<Open> = None;
    for event in batch {
        let Some(this) = (event.kind == MESSAGE_DELTA)
            .then(|| parse_delta(&event.payload))
            .flatten()
        else {
            close(&mut merged, open.take());
            merged.push(event);
            continue;
        };
        match open.take() {
            Some(mut run)
                if merged[run.row].session_id == event.session_id
                    && run.delta.message_id == this.message_id
                    && run.delta.kind == this.kind =>
            {
                run.delta.text.push_str(&this.text);
                run.deltas += 1;
                open = Some(run);
            }
            previous => {
                close(&mut merged, previous);
                merged.push(event);
                open = Some(Open {
                    row: merged.len() - 1,
                    delta: this,
                    deltas: 1,
                });
            }
        }
    }
    close(&mut merged, open);
    merged
}

/// Write one finished run's accumulated text into its row, and only when the run really merged something:
/// a lone delta keeps the bytes its producer wrote.
fn close(merged: &mut [NewEvent], run: Option<Open>) {
    if let Some(Open { row, delta, deltas }) = run
        && deltas > 1
    {
        merged[row].payload =
            serde_json::to_string(&delta).expect("a delta's three strings serialise");
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
            Self::Sessions(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for RepoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite { source, .. } => Some(source),
            Self::Sessions(error) => Some(error),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One delta, spelt the way a producer must spell it.
    fn delta(message_id: &str, kind: &str, text: &str, at: i64) -> NewEvent {
        NewEvent {
            session_id: "session".to_string(),
            kind: MESSAGE_DELTA.to_string(),
            payload: serde_json::json!({
                "message_id": message_id,
                "kind": kind,
                "text": text,
            })
            .to_string(),
            created_at: at,
        }
    }

    #[test]
    fn a_merged_row_begins_when_its_run_began_and_speaks_the_contract() {
        // Two facts about a merge that a replay can see: the row stands for the span, so its instant is the
        // first delta's, and its payload is the pinned three keys in the contract's order — a client must
        // not be able to tell a merged row from one a producer wrote (ADR-0018).
        let merged = coalesce(vec![
            delta("m1", "text", "one ", 10),
            delta("m1", "text", "two", 11),
        ]);
        assert_eq!(merged.len(), 1, "two consecutive deltas are one row");
        assert_eq!(
            merged[0].created_at, 10,
            "the row begins when its run began"
        );
        assert_eq!(
            merged[0].payload, r#"{"message_id":"m1","kind":"text","text":"one two"}"#,
            "the merged payload is not the pinned shape"
        );
    }

    #[test]
    fn a_delta_of_another_message_or_kind_is_not_merged_into_the_row_before_it() {
        // The two boundaries that are one field apart: the id a block is keyed by, and the node inside it.
        let merged = coalesce(vec![
            delta("m1", "text", "a", 1),
            delta("m2", "text", "b", 2),
            delta("m2", "thinking", "c", 3),
            delta("m1", "text", "d", 4),
        ]);
        assert_eq!(merged.len(), 4, "a boundary was crossed: {merged:#?}");
    }
}
