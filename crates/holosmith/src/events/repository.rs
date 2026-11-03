//! Every statement that reads or writes the `events` table (HS-009).
//!
//! The repository borrows the open `Database`'s connection, exactly as `sessions` does, and this
//! file is the only one with an events statement. A test below walks `src/**` and fails if one
//! appears anywhere else — and, because the seq counter lives on `sessions`, that guard also
//! proves the bump is not written here: it is `SessionRepository::bump_last_seq`, run inside this
//! file's transaction, so each repository still owns its own table (ADR-0013).

use rusqlite::{Connection, Row, params};

use super::{Event, NewEvent, RepoError};
use crate::sessions::repository::SessionRepository;

/// One row. `seq` was assigned by `bump_last_seq` inside this same transaction, so the row and
/// the counter commit together.
const INSERT: &str =
    "INSERT INTO events (session_id, seq, type, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5)";

/// The read a client makes with its last-seen seq (HS-010): one session, after `?2`, in order.
const SELECT_SINCE: &str = "SELECT session_id, seq, type, payload, created_at FROM events \
     WHERE session_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3";

/// The newest readable seq: `NULL` on an empty stream. This answers "what can a reader see",
/// which compaction can move backwards. The counter that must never move backwards is
/// `sessions.last_seq`, and seq assignment never reads this (ADR-0013).
const LATEST: &str = "SELECT max(seq) FROM events WHERE session_id = ?1";

/// The only way to read or write an `events` row. Obtained from `Database::events`.
pub struct EventRepository<'a> {
    connection: &'a Connection,
}

impl<'a> EventRepository<'a> {
    /// Borrow `connection`. `pub(crate)` rather than `pub`, so `Database::events` is the only way
    /// a caller gets one and the connection itself never leaves the database.
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    /// Insert `events` in **one** transaction and return them with the seqs it assigned.
    ///
    /// For each event the session's counter is bumped and read back, so the seq is `last_seq + 1`
    /// and never `MAX(seq) + 1`: compaction deletes rows, and a derived counter would rewind and
    /// reuse a seq. The counter bump is the sessions repository's statement, run through this
    /// transaction. All or nothing: a failure anywhere rolls the batch back, so no client ever
    /// sees a seq the batch did not commit.
    ///
    /// `pub(crate)` because the writer thread is the only caller: anything else inserting a row
    /// here could assign a seq outside the one writer, which is what "exactly one writer assigns
    /// seq" forbids (ADR-0013, PER-41 S3).
    pub(crate) fn insert_batch(&self, events: &[NewEvent]) -> Result<Vec<Event>, RepoError> {
        let sqlite = |error| RepoError::sqlite("insert_batch", error);
        // `unchecked_transaction` because the repository borrows the database's one connection
        // and cannot take `&mut` without handing the connection out. The writer holds the
        // database's mutex across this call, so nothing else in the process is writing.
        let transaction = self.connection.unchecked_transaction().map_err(sqlite)?;
        let mut assigned = Vec::with_capacity(events.len());
        for event in events {
            let seq = SessionRepository::new(&transaction)
                .bump_last_seq(&event.session_id)
                .map_err(RepoError::Sessions)?
                .ok_or_else(|| RepoError::UnknownSession(event.session_id.clone()))?;
            transaction
                .execute(
                    INSERT,
                    params![
                        event.session_id,
                        seq,
                        event.kind,
                        event.payload,
                        event.created_at
                    ],
                )
                .map_err(sqlite)?;
            assigned.push(Event::of(event, seq));
        }
        transaction.commit().map_err(sqlite)?;
        Ok(assigned)
    }

    /// Up to `limit` events of one session, after `after`, in seq order — the page a client that
    /// reconnects with a last-seen seq reads.
    pub fn read_since(
        &self,
        session_id: &str,
        after: i64,
        limit: u32,
    ) -> Result<Vec<Event>, RepoError> {
        let sqlite = |error| RepoError::sqlite("read_since", error);
        let mut statement = self.connection.prepare(SELECT_SINCE).map_err(sqlite)?;
        let rows = statement
            .query_map(params![session_id, after, limit], row_to_event)
            .map_err(sqlite)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sqlite)
    }

    /// The newest seq a reader can see, or `None` when the session has no events.
    ///
    /// Named for what it answers rather than for the counter: it is `max(seq)` over the rows that
    /// still exist, so HS-060's compaction moves it backwards, while `sessions.last_seq` is the
    /// counter that never does and that seq assignment reads (ADR-0013). No production caller
    /// today — it exists for HS-010's last-seen-seq reads and its tests.
    pub fn newest_visible_seq(&self, session_id: &str) -> Result<Option<i64>, RepoError> {
        self.connection
            .query_row(LATEST, [session_id], |row| row.get(0))
            .map_err(|error| RepoError::sqlite("newest_visible_seq", error))
    }
}

/// One row. Every column is `NOT NULL`, so the mapper cannot fail on a value.
fn row_to_event(row: &Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        session_id: row.get("session_id")?,
        seq: row.get("seq")?,
        kind: row.get("type")?,
        payload: row.get("payload")?,
        created_at: row.get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::db::{OpenOptions, apply_pending};
    use crate::schema;
    use crate::sessions::{NewSession, Session};
    use crate::sql::guard::Guard;

    /// The `created_at` and `applied_at` these tests write, so a timestamp is asserted rather
    /// than raced.
    const NOW: i64 = 1_760_000_000;

    /// The one file allowed to name the table in a statement.
    const REPOSITORY: &str = "events/repository.rs";

    /// The spellings an events statement can take, longest first so `DELETE FROM events` is
    /// reported as itself rather than as `FROM events`.
    const STATEMENTS: [&str; 4] = [
        "delete from events",
        "from events",
        "into events",
        "update events",
    ];

    /// This table's SQL-location guard: the walk and the matcher are shared with every other
    /// repository that has one (`crate::sql::guard`), so the two cannot drift or collide.
    fn guard() -> Guard {
        Guard::new("events", REPOSITORY, &STATEMENTS)
    }

    /// A migrated in-memory database, so a test reaches this repository without a writer.
    ///
    /// `insert_batch` is `pub(crate)` and the tests below are why it is still tested: the writer
    /// swallows a refused batch (it logs and drops it), so no integration test can observe the
    /// refusal — the seam has to be here (PER-41 S3).
    fn database() -> Connection {
        let mut connection = Connection::open_in_memory().expect("an in-memory database opens");
        let migrations = schema::embedded().expect("the embedded migrations are usable");
        apply_pending(
            &mut connection,
            Path::new(":memory:"),
            &migrations,
            OpenOptions::default(),
            NOW,
        )
        .expect("a fresh in-memory database migrates");
        connection
    }

    fn create_session(connection: &Connection, client_key: &str) -> Session {
        SessionRepository::new(connection)
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

    fn new_event(session: &Session, kind: &str) -> NewEvent {
        NewEvent {
            session_id: session.id.clone(),
            kind: kind.to_string(),
            payload: format!("{{\"kind\":\"{kind}\"}}"),
            created_at: NOW,
        }
    }

    #[test]
    fn no_events_sql_outside_the_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let guard = guard();
        assert!(
            guard
                .sources_under(&root)
                .iter()
                .any(|(file, _)| file == REPOSITORY),
            "the walk did not find {root:?}/{REPOSITORY}"
        );
        let offenders = guard.offenders_under(&root);
        assert!(
            offenders.is_empty(),
            "an events statement lives outside src/{REPOSITORY}, which owns every read and \
             write of the table: {offenders:?}"
        );
    }

    #[test]
    fn the_guard_fires_on_a_planted_statement() {
        // The guard above passes over today's tree, so this one plants a tree where it must fire.
        // The other files are the shapes the matcher has to leave alone; that the sessions
        // repository is one of them is what `no_events_sql_outside_the_repository` proves over
        // the real tree, which holds the counter bump.
        let planted = guard().temp_dir("planted");
        planted.plant("planted.rs", "let sql = \"SELECT seq FROM events\";\n");
        planted.plant("harmless.rs", "let events = 1;\n");
        planted.plant("suffix.rs", "let sql = \"SELECT * FROM events_notes\";\n");
        // A `\`-continuation after no space is not a break in Rust either: the two lines are the
        // one token `FROMevents` at runtime, which is not a statement.
        planted.plant(
            "glued.rs",
            "let sql = \"SELECT seq FROM\\\n     events\";\n",
        );
        planted.plant(
            "rename.rs",
            "let sql = \"ALTER TABLE old_events RENAME TO events\";\n",
        );
        planted.plant(
            "index.rs",
            "let sql = \"CREATE INDEX events_recorded ON events (created_at)\";\n",
        );
        assert_eq!(
            guard().offenders_under(planted.path()),
            ["planted.rs:1: from events"]
        );
    }

    #[test]
    fn the_guard_sees_through_the_spellings_that_hide_a_statement() {
        // Each spelling is one statement at runtime while the literal phrase in `STATEMENTS`
        // never appears on its line: a schema prefix, identifier quoting, a conflict clause and
        // a Rust `\`-continuation. One planted file each, so the reported line is the one the
        // statement was written on.
        let guard = guard();
        for (file, text, reported) in [
            (
                "prefix.rs",
                "let sql = \"SELECT seq FROM main.events\";\n",
                "prefix.rs:1: from events",
            ),
            (
                "quoted.rs",
                "let sql = \"SELECT seq FROM \\\"events\\\"\";\n",
                "quoted.rs:1: from events",
            ),
            (
                "bracketed.rs",
                "let sql = \"DELETE FROM [events]\";\n",
                "bracketed.rs:1: delete from events",
            ),
            (
                "conflict.rs",
                "let sql = \"UPDATE OR REPLACE events SET seq = 0\";\n",
                "conflict.rs:1: update events",
            ),
            (
                "continued.rs",
                "let sql = \"SELECT seq FROM \\\n     events\";\n",
                "continued.rs:1: from events",
            ),
        ] {
            let planted = guard.temp_dir("spelling");
            planted.plant(file, text);
            assert_eq!(guard.offenders_under(planted.path()), [reported], "{file}");
        }
    }

    #[test]
    fn read_since_answers_after_the_given_seq_in_order() {
        let connection = database();
        let session = create_session(&connection, "read-since");
        let repository = EventRepository::new(&connection);
        let events: Vec<NewEvent> = ["first", "second", "third"]
            .into_iter()
            .map(|kind| new_event(&session, kind))
            .collect();
        let assigned = repository
            .insert_batch(&events)
            .expect("the batch is inserted");
        assert_eq!(
            assigned.iter().map(|event| event.seq).collect::<Vec<i64>>(),
            [1, 2, 3]
        );

        let after = repository
            .read_since(&session.id, 1, 10)
            .expect("the page answers");
        assert_eq!(
            after.iter().map(|event| event.seq).collect::<Vec<i64>>(),
            [2, 3]
        );
        let limited = repository
            .read_since(&session.id, 1, 1)
            .expect("the page answers");
        assert_eq!(
            limited.iter().map(|event| event.seq).collect::<Vec<i64>>(),
            [2]
        );
        assert_eq!(
            repository
                .newest_visible_seq(&session.id)
                .expect("the newest visible seq answers"),
            Some(3)
        );
    }

    #[test]
    fn an_event_for_an_unknown_session_commits_nothing() {
        let connection = database();
        let repository = EventRepository::new(&connection);
        let error = repository
            .insert_batch(&[NewEvent {
                session_id: "0123456789abcdef".to_string(),
                kind: "status".to_string(),
                payload: "{}".to_string(),
                created_at: NOW,
            }])
            .expect_err("an event needs a session to carry its counter");
        assert!(
            matches!(error, RepoError::UnknownSession(ref id) if id == "0123456789abcdef"),
            "{error}"
        );
        assert!(
            error.to_string().contains("no session"),
            "the error does not name the cause: {error}"
        );
        assert!(
            repository
                .read_since("0123456789abcdef", 0, 10)
                .expect("the read answers")
                .is_empty()
        );
    }
}
