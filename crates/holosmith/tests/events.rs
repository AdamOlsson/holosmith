//! The event log and its single writer (HS-009, ADR-0013).
//!
//! Every test drives the shipped path: a real `Database` on a temporary file, the writer thread
//! `events::spawn` starts, and — where the rule is about visibility — a *second* connection
//! reading the same file. Counting rows through the writer's own connection would prove nothing
//! about what another reader can see.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tokio::sync::broadcast::error::RecvError;

use holosmith::db::{Database, OpenOptions};
use holosmith::events::{self, Event, EventStream, EventWriter, NewEvent, WriterOptions};
use holosmith::sessions::{NewSession, Session};

mod support;
use support::TempDir;

/// The `created_at` every test writes, so a timestamp is asserted rather than raced.
const NOW: i64 = 1_760_000_000;

/// A short commit window: long enough that a burst lands in one batch on a busy machine, short
/// enough that a test does not wait on it.
const INTERVAL: Duration = Duration::from_millis(50);

/// How long a test waits for something the writer thread does before calling it stuck.
const PATIENCE: Duration = Duration::from_secs(10);

/// One database, its writer, and one subscriber.
struct Writer {
    database: Arc<Mutex<Database>>,
    handle: EventWriter,
    stream: EventStream,
}

fn writer(dir: &TempDir, commit_interval: Duration) -> Writer {
    let database = Arc::new(Mutex::new(
        Database::open(&dir.database(), OpenOptions::default(), NOW)
            .expect("a fresh database opens"),
    ));
    let (handle, stream) = events::spawn(Arc::clone(&database), WriterOptions { commit_interval });
    Writer {
        database,
        handle,
        stream,
    }
}

impl Writer {
    /// A session for events to hang off: `seq` is `sessions.last_seq`, so a row must exist.
    fn create_session(&self, client_key: &str) -> Session {
        let database = self.database.lock().expect("the lock is not poisoned");
        database
            .sessions()
            .create(
                &NewSession {
                    project: "holosmith".to_string(),
                    model: "claude-sonnet-4.5".to_string(),
                    thinking: "high".to_string(),
                    network: false,
                    client_key: Some(client_key.to_string()),
                    base_commit: None,
                },
                NOW,
            )
            .expect("a session is created")
    }

    async fn append(&self, session: &Session, kind: &str) {
        self.handle
            .append(NewEvent {
                session_id: session.id.clone(),
                kind: kind.to_string(),
                payload: format!("{{\"kind\":\"{kind}\"}}"),
                created_at: NOW,
            })
            .await
            .expect("the writer accepts the event");
    }

    /// The next `count` committed events, in the order they were broadcast.
    async fn take(&mut self, count: usize) -> Vec<Event> {
        let mut events = Vec::with_capacity(count);
        for _ in 0..count {
            let event = tokio::time::timeout(PATIENCE, self.stream.recv())
                .await
                .expect("the writer commits within the deadline")
                .expect("a committed event arrives");
            events.push(event);
        }
        events
    }
}

/// A second connection to the same file: what another reader — a subscriber, a restart, a
/// compaction job — sees.
fn reader(dir: &TempDir) -> Connection {
    let connection = Connection::open(dir.database()).expect("a second connection opens");
    connection
        .busy_timeout(Duration::from_secs(5))
        .expect("a busy timeout is settable");
    connection
}

fn committed(connection: &Connection, session_id: &str) -> i64 {
    connection
        .query_row(
            "SELECT count(*) FROM events WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .expect("the count answers")
}

fn pragma<T: rusqlite::types::FromSql>(connection: &Connection, name: &str) -> T {
    connection
        .pragma_query_value(None, name, |row| row.get(0))
        .expect("the pragma answers")
}

#[test]
fn the_events_table_has_every_column_and_the_composite_key() {
    let dir = TempDir::new("events-schema");
    let database = support::open(&dir);
    let connection = reader(&dir);
    drop(database);

    let columns: Vec<(String, String, i64, i64)> = connection
        .prepare("PRAGMA table_info(events)")
        .expect("table_info is preparable")
        .query_map([], |row| {
            Ok((
                row.get("name")?,
                row.get("type")?,
                row.get("notnull")?,
                row.get("pk")?,
            ))
        })
        .expect("table_info answers")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("every column reads");
    assert_eq!(
        columns,
        [
            ("session_id".to_string(), "TEXT".to_string(), 1, 1),
            ("seq".to_string(), "INTEGER".to_string(), 1, 2),
            ("type".to_string(), "TEXT".to_string(), 1, 0),
            ("payload".to_string(), "TEXT".to_string(), 1, 0),
            ("created_at".to_string(), "INTEGER".to_string(), 1, 0),
        ],
        "the events table is not the story's five columns with (session_id, seq) as its key"
    );

    // The read order is SQLite's own index for the composite primary key, so no second, named
    // index over the same columns exists (PER-41 S4). The name is read from `index_list` rather
    // than written here: SQLite numbers its autoindexes (`sqlite_autoindex_events_1`), and the
    // assertion should name the index it actually built.
    let primary_key_index = connection
        .prepare("PRAGMA index_list(events)")
        .expect("index_list is preparable")
        .query_map([], |row| {
            Ok((
                row.get::<_, String>("name")?,
                row.get::<_, String>("origin")?,
            ))
        })
        .expect("index_list answers")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("every index reads")
        .into_iter()
        .find_map(|(name, origin)| (origin == "pk").then_some(name))
        .expect("the composite primary key has an index");
    assert!(
        primary_key_index.starts_with("sqlite_autoindex_events"),
        "the primary key's index is {primary_key_index}"
    );
    let indexed: Vec<(i64, String)> = connection
        .prepare(&format!("PRAGMA index_info({primary_key_index})"))
        .expect("index_info is preparable")
        .query_map([], |row| Ok((row.get("seqno")?, row.get("name")?)))
        .expect("index_info answers")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("every index column reads");
    assert_eq!(
        indexed,
        [(0, "session_id".to_string()), (1, "seq".to_string()),],
        "the primary key's index does not cover (session_id, seq)"
    );

    // Uniqueness is the composite primary key, proved by inserting the same pair twice.
    let insert = "INSERT INTO events (session_id, seq, type, payload, created_at) \
                  VALUES ('0123456789abcdef', 1, 'status', '{}', 0)";
    connection
        .execute(insert, [])
        .expect("the first row is insertable");
    let duplicate = connection
        .execute(insert, [])
        .expect_err("a duplicate (session_id, seq) is refused");
    assert!(
        matches!(
            duplicate,
            rusqlite::Error::SqliteFailure(ref error, _)
                if error.code == rusqlite::ffi::ErrorCode::ConstraintViolation
        ),
        "{duplicate}"
    );
    assert!(
        duplicate
            .to_string()
            .contains("UNIQUE constraint failed: events.session_id, events.seq"),
        "{duplicate}"
    );
}

#[test]
fn the_database_runs_wal_and_full_sync() {
    // The pragmas are set and read back by `Database::open` before this story existed (ADR-0009);
    // this re-proves them on the file the writer uses rather than reimplementing the read-back.
    // The two halves are not equally proved, and the difference is what each line below says:
    //
    // - `journal_mode` is a property of the file and outlives the connection, so `wal` on a
    //   second connection is genuine proof that the database is in WAL.
    // - `synchronous` is per-connection and SQLite's default is already FULL(2), so asserting 2
    //   on this fresh connection would pass whatever the startup did. What is proved here is
    //   weaker and is stated as what it is: `open` returned `Ok`, so the read-back inside
    //   `configured` saw 2 instead of failing the start with `DbError::Pragma`. That read-back is
    //   made falsifiable in `db.rs`'s `the_four_pragmas_read_back_on_a_real_file`, which sets OFF
    //   first; re-asserting 2 here would only repeat the default.
    let dir = TempDir::new("events-pragmas");
    let database = Database::open(&dir.database(), OpenOptions::default(), NOW)
        .expect("a fresh database opens");
    let connection = reader(&dir);
    let journal_mode: String = pragma(&connection, "journal_mode");
    assert_eq!(journal_mode, "wal", "the file is not in WAL");
    drop(database);
}

#[tokio::test]
async fn one_batch_is_one_transaction() {
    // A long window so the read below lands inside the batch, not after it.
    let dir = TempDir::new("events-batch");
    let mut writer = writer(&dir, Duration::from_secs(1));
    let session = writer.create_session("one-batch");
    for kind in ["first", "second", "third"] {
        writer.append(&session, kind).await;
    }

    // The transaction is still open: a second connection sees none of the three. Counting rows
    // through the writer's own connection would prove nothing here.
    let connection = reader(&dir);
    assert_eq!(
        committed(&connection, &session.id),
        0,
        "an event was committed before its batch"
    );

    // The first event to arrive brings the whole batch with it: all three rows are committed
    // before any of them is broadcast.
    let first = writer.take(1).await.remove(0);
    assert_eq!(first.seq, 1);
    assert_eq!(
        committed(&connection, &session.id),
        3,
        "the batch did not commit as one"
    );

    // And they arrive in the order one transaction assigned.
    let rest = writer.take(2).await;
    let seqs: Vec<i64> = rest.iter().map(|event| event.seq).collect();
    assert_eq!(seqs, [2, 3]);
}

#[tokio::test]
async fn events_arrive_in_seq_order() {
    let dir = TempDir::new("events-order");
    let mut writer = writer(&dir, INTERVAL);
    let session = writer.create_session("order");
    let kinds = ["starting", "idle", "running", "idle", "stopped"];
    for kind in kinds {
        writer.append(&session, kind).await;
    }

    let events = writer.take(kinds.len()).await;
    let seqs: Vec<i64> = events.iter().map(|event| event.seq).collect();
    let received: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
    assert_eq!(seqs, (1..=kinds.len() as i64).collect::<Vec<i64>>());
    assert_eq!(
        received, kinds,
        "the broadcast order is not the commit order"
    );
    assert!(events.iter().all(|event| event.session_id == session.id));
}

#[tokio::test]
async fn seq_is_strictly_increasing_per_session() {
    let dir = TempDir::new("events-per-session");
    let mut writer = writer(&dir, INTERVAL);
    let first = writer.create_session("first");
    let second = writer.create_session("second");
    // Interleaved, so a counter shared by the table would show up as a gap in one session.
    for (session, kind) in [
        (&first, "a1"),
        (&second, "b1"),
        (&first, "a2"),
        (&second, "b2"),
        (&first, "a3"),
    ] {
        writer.append(session, kind).await;
    }

    let events = writer.take(5).await;
    let seqs = |id: &str| -> Vec<i64> {
        events
            .iter()
            .filter(|event| event.session_id == id)
            .map(|event| event.seq)
            .collect()
    };
    assert_eq!(seqs(&first.id), [1, 2, 3]);
    assert_eq!(seqs(&second.id), [1, 2]);
    assert!(seqs(&first.id).windows(2).all(|pair| pair[0] < pair[1]));
    assert!(seqs(&second.id).windows(2).all(|pair| pair[0] < pair[1]));
}

#[tokio::test]
async fn seq_is_not_reused_after_the_highest_row_is_deleted() {
    let dir = TempDir::new("events-compaction");
    let mut writer = writer(&dir, INTERVAL);
    let session = writer.create_session("compaction");
    for kind in ["first", "second", "third"] {
        writer.append(&session, kind).await;
    }
    writer.take(3).await;

    // What HS-060's compaction does: the rows go, the counter does not. `MAX(seq) + 1` would
    // answer 1 here and reuse three seqs the client has already seen.
    let connection = reader(&dir);
    connection
        .execute("DELETE FROM events WHERE session_id = ?1", [&session.id])
        .expect("the rows are deletable");
    assert_eq!(committed(&connection, &session.id), 0);

    writer.append(&session, "after compaction").await;
    let event = writer.take(1).await.remove(0);
    assert_eq!(
        event.seq, 4,
        "seq was reused after the highest row was deleted"
    );

    let stored = {
        let database = writer.database.lock().expect("the lock is not poisoned");
        database
            .events()
            .read_since(&session.id, 0, 10)
            .expect("the events are readable")
    };
    assert_eq!(
        stored,
        [event],
        "the surviving row is not the one broadcast"
    );
}

#[tokio::test]
async fn a_subscriber_sees_the_row_before_the_event() {
    let dir = TempDir::new("events-commit-first");
    // A long commit window, so the test *sits inside* the batch instead of racing it: with a
    // broken send-before-commit the event arrives while the transaction is still open, which is
    // an observable failure rather than a microsecond the test would have to lose.
    let window = Duration::from_millis(500);
    let mut writer = writer(&dir, window);
    let session = writer.create_session("commit-first");
    writer.append(&session, "status").await;

    // Nothing is broadcast while the batch is open, and a second connection sees no row: either
    // half of a send-before-commit fails one of these two assertions. Counting rows through the
    // writer's own connection would prove nothing — the read has to be a different reader.
    let connection = reader(&dir);
    match tokio::time::timeout(Duration::from_millis(100), writer.stream.recv()).await {
        Err(_elapsed) => {}
        Ok(other) => panic!("an event was broadcast before its batch committed: {other:?}"),
    }
    assert_eq!(
        committed(&connection, &session.id),
        0,
        "an event row was committed before its batch"
    );

    // The window expires, the batch commits, and only then does the event arrive — with the row
    // already visible to the second connection.
    let event = writer.take(1).await.remove(0);
    let payload: String = connection
        .query_row(
            "SELECT payload FROM events WHERE session_id = ?1 AND seq = ?2",
            rusqlite::params![&event.session_id, event.seq],
            |row| row.get(0),
        )
        .expect("the row is visible when the event is delivered");
    assert_eq!(payload, event.payload);
}

#[tokio::test]
async fn the_writer_handle_can_make_a_subscriber() {
    // `spawn`'s stream is the only one besides this, and `serve` drops it. Without a way to
    // subscribe through the handle, the broadcast half would be unreachable from the
    // `EventWriter` an `AppState` carries, which is what HS-010 lands on (PER-41 P1).
    let dir = TempDir::new("events-subscribe");
    let mut writer = writer(&dir, INTERVAL);
    let mut subscriber = writer.handle.subscribe();
    let session = writer.create_session("subscribe");
    writer.append(&session, "status").await;

    let event = tokio::time::timeout(PATIENCE, subscriber.recv())
        .await
        .expect("the writer commits within the deadline")
        .expect("a committed event arrives");
    assert_eq!(event.seq, 1);
    let connection = reader(&dir);
    assert_eq!(committed(&connection, &session.id), 1);
    // An independent subscriber, so both received the one committed event.
    assert_eq!(writer.take(1).await.remove(0), event);
}

#[tokio::test]
async fn a_lagged_subscriber_does_not_stall_the_writer() {
    let dir = TempDir::new("events-lagged");
    let mut writer = writer(&dir, Duration::from_millis(5));
    let session = writer.create_session("lagged");
    // More than the broadcast buffer, and the subscriber is never polled while they are queued.
    let total: i64 = 1_100;
    for index in 0..total {
        writer.append(&session, &format!("event-{index}")).await;
    }

    // Every one committed anyway: a slow subscriber cannot hold up a commit.
    let connection = reader(&dir);
    let deadline = Instant::now() + PATIENCE;
    loop {
        let committed = committed(&connection, &session.id);
        if committed == total {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the writer committed {committed} of {total} events with no subscriber draining"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // Sends are asynchronous to the commit, so wait until the writer has certainly finished
    // sending before draining: 1100 non-blocking sends take far less than this.
    std::thread::sleep(Duration::from_millis(250));

    // The subscriber is told it fell behind rather than being silently wrong (HS-010 re-fetches
    // those events by last-seen seq).
    match writer.stream.recv().await {
        Err(RecvError::Lagged(missed)) => assert!(
            missed >= (total - 1024) as u64,
            "lagged by {missed}, but the buffer holds 1024 of {total}"
        ),
        other => panic!("expected a lag report, got {other:?}"),
    }
    // The ring buffer holds the newest events, so the stream carries on from the oldest one it
    // still has, in order, and reaches the last event the writer sent.
    let mut last = 0;
    loop {
        match writer.stream.recv().await {
            Ok(event) => {
                assert!(event.seq > last, "{event:?} follows seq {last}");
                last = event.seq;
                if last == total {
                    break;
                }
            }
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => panic!("the stream closed"),
        }
    }
    assert_eq!(last, total);
}

#[tokio::test]
async fn the_durability_rule_holds_across_a_reopen() {
    let dir = TempDir::new("events-durability");
    let mut writer = writer(&dir, INTERVAL);
    let session = writer.create_session("durability");
    for kind in ["first", "second", "third"] {
        writer.append(&session, kind).await;
    }
    let broadcast = writer.take(3).await;

    // Drop the producer, then wait for the thread to stop: the stream answers `Closed` when the
    // broadcast sender is dropped with the thread, so the connection is gone before the reopen.
    let Writer {
        database,
        handle,
        mut stream,
    } = writer;
    drop(handle);
    match tokio::time::timeout(PATIENCE, stream.recv())
        .await
        .expect("the writer stops within the deadline")
    {
        Err(RecvError::Closed) => {}
        other => panic!("expected the writer to stop, got {other:?}"),
    }
    drop(stream);
    drop(database);

    // A restart: the same file, opened and migrated again, must carry every seq that was
    // broadcast, with the same payload.
    let reopened = Database::open(&dir.database(), OpenOptions::default(), NOW + 60)
        .expect("the database reopens");
    let stored = reopened
        .events()
        .read_since(&session.id, 0, 10)
        .expect("the events are readable");
    assert_eq!(stored, broadcast);
    assert_eq!(
        reopened
            .events()
            .newest_visible_seq(&session.id)
            .expect("the newest visible seq answers"),
        Some(3)
    );

    // The rows surviving is half the rule; the *counter* is the half spec item 5's rationale is
    // about — "a restarted seq counter would make clients drop new events". Append after the
    // reopen and assert the next seq is 4: a counter derived from the rows, or one that reset,
    // would reuse 1.
    let reopened = Arc::new(Mutex::new(reopened));
    let (restarted, mut stream) = events::spawn(
        Arc::clone(&reopened),
        WriterOptions {
            commit_interval: INTERVAL,
        },
    );
    restarted
        .append(NewEvent {
            session_id: session.id.clone(),
            kind: "after the restart".to_string(),
            payload: "{}".to_string(),
            created_at: NOW + 60,
        })
        .await
        .expect("the restarted writer accepts the event");
    let event = tokio::time::timeout(PATIENCE, stream.recv())
        .await
        .expect("the restarted writer commits within the deadline")
        .expect("a committed event arrives");
    assert_eq!(
        event.seq, 4,
        "the seq counter did not survive the reopen: the next seq was {}",
        event.seq
    );
    let connection = reader(&dir);
    assert_eq!(
        committed(&connection, &session.id),
        4,
        "the appended event is not the fourth row"
    );
}
