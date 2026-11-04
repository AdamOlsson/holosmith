//! Every statement that reads or writes the `sessions` table (HS-007).
//!
//! The repository borrows the open `Database`'s connection rather than owning one. The connection
//! reaches a handler behind ADR-0012's `Arc<Mutex<Database>>` stopgap until HS-009's single writer
//! owns the write path; either way this file is the only one with a sessions statement. A test
//! below walks `src/**` and fails if a sessions statement appears anywhere else, which is what
//! makes "no SQL outside the repository" checkable rather than a review note.

use std::str::FromStr;

use rusqlite::types::ToSql;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{NewSession, RepoError, Session, Status};

/// Every column, in the order `row_to_session` reads them.
const COLUMNS: &str = "id, project, model, thinking, worktree, base_commit, status, status_reason, \
     client_key, network, created_at, last_active_at, stopped_at, usage_in, usage_out";

/// The id: 16 lowercase hex characters from the database that stores them, so no crate is added
/// for a uuid and no PRNG is hand-rolled (ADR-0011).
const NEW_ID: &str = "SELECT lower(hex(randomblob(8)))";

/// A new row: `worktree` is `''` until the provisioner runs, and `?9` is both `created_at` and
/// `last_active_at`.
const INSERT: &str = "INSERT INTO sessions \
     (id, project, model, thinking, worktree, base_commit, status, status_reason, client_key, \
      network, created_at, last_active_at, stopped_at, usage_in, usage_out) \
     VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, NULL, ?7, ?8, ?9, ?9, NULL, 0, 0)";

/// A status change: `stopped_at` moves with the status, `last_active_at` does not (it is
/// activity, and HS-020/HS-021 own when it moves).
const UPDATE_STATUS: &str =
    "UPDATE sessions SET status = ?2, status_reason = ?3, stopped_at = ?4 WHERE id = ?1";

/// One bit of activity: `last_active_at` moves and nothing else (HS-015's prompt and settle, and
/// HS-017's steer).
const TOUCH: &str = "UPDATE sessions SET last_active_at = ?2 WHERE id = ?1";

/// The row's removal: what `deleting` means once the session owns nothing any more (HS-019).
const REMOVE: &str = "DELETE FROM sessions WHERE id = ?1";

/// The event counter's bump: one per event, inside the event insert's transaction, returning the
/// value it assigned (HS-009). A column, not `MAX(seq)`, because compaction deletes rows and a
/// derived counter would rewind and reuse a seq (ADR-0013).
const BUMP_LAST_SEQ: &str =
    "UPDATE sessions SET last_seq = last_seq + 1 WHERE id = ?1 RETURNING last_seq";

/// The order `list` answers in: newest activity first, for the Sessions screen (HS-013), and the
/// id after it so the order is total. The same `sessions_last_active` index also serves
/// eviction's ascending read of the least recently active session (HS-020).
const ORDER: &str = "ORDER BY last_active_at DESC, id ASC";

/// The cap's count: the three live statuses, supplied from `Status::as_str` so the words stay in
/// the enum, over the `sessions_status` index (ADR-0012).
const LIVE_COUNT: &str = "SELECT count(*) FROM sessions WHERE status IN (?1, ?2, ?3)";

/// The eviction candidate's filter: the only row it may be chosen from, and the order that makes the
/// choice total. A filter rather than a whole statement, so the reader stays [`select`]'s and a column
/// added by a migration cannot be missed here while every other read keeps working.
const OLDEST_IDLE: &str = "WHERE status = ?1 ORDER BY last_active_at ASC, id ASC LIMIT 1";

/// The only way to read or write a `sessions` row. Obtained from `Database::sessions`.
pub struct SessionRepository<'a> {
    connection: &'a Connection,
}

impl<'a> SessionRepository<'a> {
    /// Borrow `connection`. `pub(crate)` rather than `pub`, so `Database::sessions` is the only
    /// way a caller gets one and the connection itself never leaves the database.
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    /// Insert a session in `starting` and return the row as stored.
    ///
    /// The id is generated before the insert, not by a column default, so the caller can log it
    /// and a test can assert its shape.
    pub fn create(&self, new: &NewSession, now: i64) -> Result<Session, RepoError> {
        let sqlite = |error| RepoError::sqlite("create", error);
        let id: String = self
            .connection
            .query_row(NEW_ID, [], |row| row.get(0))
            .map_err(sqlite)?;
        self.connection
            .execute(
                INSERT,
                params![
                    &id,
                    new.project,
                    new.model,
                    new.thinking,
                    new.base_commit,
                    // The table's `none` row, not a second spelling of "a create starts in
                    // starting" (HS-012). `initial_status` panics only if the table loses its
                    // create row, which its own test refuses.
                    super::state::initial_status().as_str(),
                    new.client_key,
                    new.network,
                    now,
                ],
            )
            .map_err(|error| {
                if is_duplicate_key(&error) {
                    RepoError::DuplicateClientKey
                } else {
                    sqlite(error)
                }
            })?;
        // Read the row back rather than assembling what was written: the returned session is the
        // one a later `get` sees. `query_row` makes "the row vanished" an error, not an `Option`
        // this call would have to unwrap.
        self.connection
            .query_row(&select("WHERE id = ?1"), [&id], row_to_session)
            .map_err(sqlite)?
    }

    /// One session by its id.
    pub fn get(&self, id: &str) -> Result<Option<Session>, RepoError> {
        self.one("get", "WHERE id = ?1", &[&id])
    }

    /// One session by the idempotency key its create carried (HS-008).
    pub fn by_client_key(&self, client_key: &str) -> Result<Option<Session>, RepoError> {
        self.one("by_client_key", "WHERE client_key = ?1", &[&client_key])
    }

    /// Every session, or every session in one status, in `ORDER`.
    pub fn list(&self, status: Option<Status>) -> Result<Vec<Session>, RepoError> {
        let sqlite = |error| RepoError::sqlite("list", error);
        let sql = match status {
            Some(_) => select(&format!("WHERE status = ?1 {ORDER}")),
            None => select(ORDER),
        };
        let mut statement = self.connection.prepare(&sql).map_err(sqlite)?;
        let rows = match status {
            Some(status) => statement.query_map([status.as_str()], row_to_session),
            None => statement.query_map([], row_to_session),
        }
        .map_err(sqlite)?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sqlite)?;
        rows.into_iter().collect()
    }

    /// How many sessions are live — `starting`, `idle` or `running` — the set item 8's cap
    /// counts (ADR-0012). One statement over the `sessions_status` index, not `list` with three
    /// filters: a count is one row of one column, and the three-status set is HS-020's to change.
    pub fn live_count(&self) -> Result<u64, RepoError> {
        self.connection
            .query_row(
                LIVE_COUNT,
                params![
                    Status::Starting.as_str(),
                    Status::Idle.as_str(),
                    Status::Running.as_str(),
                ],
                |row| row.get(0),
            )
            .map_err(|error| RepoError::sqlite("live_count", error))
    }

    /// The least recently active `idle` session: the cap's victim (HS-020), or `None` when nothing is
    /// evictable.
    ///
    /// One statement rather than `list(Some(Status::Idle)).last()`: the answer is one row, and the
    /// ordering is the same `last_active_at` the list uses, so "least recently active" has one
    /// definition. A `starting` or `running` session is live but never a candidate — stopping one
    /// would destroy a turn nobody asked to end (the story's third bullet), and `stopped`, `error` and
    /// `deleting` hold no slot to free.
    pub fn oldest_idle(&self) -> Result<Option<Session>, RepoError> {
        self.one(
            "oldest_idle",
            OLDEST_IDLE,
            &[&Status::Idle.as_str() as &dyn ToSql],
        )
    }

    /// Set the status and its reason, and move `stopped_at` with it: `now` when the new status is
    /// `stopped`, `NULL` otherwise, which is item 15's "cleared on resume".
    ///
    /// **Not the way to change a state.** Since HS-012 the one production caller is
    /// [`super::state::Transitioner::apply`], which decides whether the move is in the table and
    /// emits the `session.status` event; this method only writes. A source-level guard in
    /// `sessions/state.rs` fails the build's tests if any other file under `src/` names it.
    ///
    /// `None` when there is no such id, so a caller can answer 404 without a second read.
    pub fn set_status(
        &self,
        id: &str,
        status: Status,
        status_reason: Option<&str>,
        now: i64,
    ) -> Result<Option<Session>, RepoError> {
        let stopped_at = (status == Status::Stopped).then_some(now);
        let updated = self
            .connection
            .execute(
                UPDATE_STATUS,
                params![id, status.as_str(), status_reason, stopped_at],
            )
            .map_err(|error| RepoError::sqlite("set_status", error))?;
        if updated == 0 {
            return Ok(None);
        }
        self.one("set_status", "WHERE id = ?1", &[&id])
    }

    /// Record that this session was active at `now` (HS-015, HS-020/HS-021's LRU order).
    ///
    /// Activity is not a state change, so this is the repository's and not the transitioner's: the
    /// status write deliberately leaves `last_active_at` alone, and the callers that mean "something
    /// happened" (`prompt`, `agent_settled`, `steer`) say so here.
    ///
    /// **A missing row is not an error.** The caller is a background task that is touching a session
    /// it just moved, and a session it can no longer touch (one deleted or recovered underneath it)
    /// has nothing to record — an `Err` would turn that ordinary race into a logged failure. Nothing
    /// reads the row back, so there is no `Option<Session>` to answer: `Ok` means the statement ran.
    pub fn touch(&self, id: &str, now: i64) -> Result<(), RepoError> {
        self.connection
            .execute(TOUCH, params![id, now])
            .map_err(|error| RepoError::sqlite("touch", error))?;
        Ok(())
    }

    /// Remove this session's row and its event log, in **one** transaction (HS-019).
    ///
    /// The log goes first and through the events repository's own statement, so no `events` SQL
    /// lives here; that statement borrows the transaction this call opened, exactly as
    /// `EventRepository::insert_batch` borrows it for the sessions repository's counter bump. One
    /// transaction, because a session whose log was half-removed is a session whose transcript
    /// would jump backwards on a reconnect.
    ///
    /// **The last thing a delete does**, and only after the backend has said it owns nothing any
    /// more: the row is what names the resources, so removing it first would hide them.
    ///
    /// `false` when there is no such row — a second delete after the first finished, which the
    /// caller answers as `not_found`.
    pub fn remove(&self, id: &str) -> Result<bool, RepoError> {
        let sqlite = |error| RepoError::sqlite("remove", error);
        let transaction = self.connection.unchecked_transaction().map_err(sqlite)?;
        // The events statement answers SQLite's own error rather than this module's, because it is
        // the other repository's and its one failure is the database's; the operation the caller
        // asked for is this one, so the error names it.
        crate::events::repository::EventRepository::new(&transaction)
            .delete_session_log(id)
            .map_err(sqlite)?;
        let removed = transaction.execute(REMOVE, [id]).map_err(sqlite)?;
        transaction.commit().map_err(sqlite)?;
        Ok(removed > 0)
    }

    /// Bump and return this session's event counter (HS-009).
    ///
    /// The caller runs it inside the event insert's transaction, so the seq a row carries and the
    /// counter that assigned it commit together; `None` means there is no such session. It lives
    /// here because every `sessions` statement does, and the events repository calls it through
    /// the transaction rather than writing the table itself (ADR-0013).
    ///
    /// `pub(crate)` because there is exactly one caller and one writer: "exactly one thread
    /// assigns seq" would be convention if anything holding the database's lock could bump the
    /// counter on its own and leave a permanent gap in a session's stream (PER-41 S3).
    pub(crate) fn bump_last_seq(&self, id: &str) -> Result<Option<i64>, RepoError> {
        self.connection
            .query_row(BUMP_LAST_SEQ, [id], |row| row.get(0))
            .optional()
            .map_err(|error| RepoError::sqlite("bump_last_seq", error))
    }

    /// The row a filter matches, or `None`: the reads that must not fail on "no such row".
    /// `operation` is the public method the statement belongs to, so the error names it.
    fn one(
        &self,
        operation: &'static str,
        filter: &str,
        parameters: &[&dyn ToSql],
    ) -> Result<Option<Session>, RepoError> {
        self.connection
            .query_row(&select(filter), parameters, row_to_session)
            .optional()
            .map_err(|error| RepoError::sqlite(operation, error))?
            .transpose()
    }
}

/// `SELECT <every column> FROM sessions <filter>`, so the readers cannot drift from `COLUMNS`.
fn select(filter: &str) -> String {
    format!("SELECT {COLUMNS} FROM sessions {filter}")
}

/// Whether `error` is SQLite's `UNIQUE` constraint violation, which on this table can only be
/// `client_key` (ADR-0011). The extended code is what separates it from every other constraint:
/// `ErrorCode` answers only `ConstraintViolation`, which a `NOT NULL` failure produces too. A
/// message match would pass a naive test and break on a SQLite upgrade.
fn is_duplicate_key(error: &rusqlite::Error) -> bool {
    error.sqlite_error().map(|error| error.extended_code)
        == Some(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE)
}

/// One row, with `status` parsed.
///
/// The parse is fallible and `Row` is not, so the mapper answers `Result<Session, RepoError>`
/// inside the `rusqlite` result and each caller transposes it — which is what keeps a corrupt
/// row's offending text inside `RepoError::Status` instead of being flattened into a SQLite error.
fn row_to_session(row: &Row<'_>) -> rusqlite::Result<Result<Session, RepoError>> {
    let status = match Status::from_str(&row.get::<_, String>("status")?) {
        Ok(status) => status,
        Err(error) => return Ok(Err(error)),
    };
    Ok(Ok(Session {
        id: row.get("id")?,
        project: row.get("project")?,
        model: row.get("model")?,
        thinking: row.get("thinking")?,
        worktree: row.get("worktree")?,
        base_commit: row.get("base_commit")?,
        status,
        status_reason: row.get("status_reason")?,
        client_key: row.get("client_key")?,
        network: row.get("network")?,
        created_at: row.get("created_at")?,
        last_active_at: row.get("last_active_at")?,
        stopped_at: row.get("stopped_at")?,
        usage_in: row.get("usage_in")?,
        usage_out: row.get("usage_out")?,
    }))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::sql::guard::Guard;

    /// The one file allowed to name the table in a statement.
    const REPOSITORY: &str = "sessions/repository.rs";

    /// The spellings a sessions statement can take, longest first so `DELETE FROM sessions` is
    /// reported as itself rather than as `FROM sessions`.
    const STATEMENTS: [&str; 4] = [
        "delete from sessions",
        "from sessions",
        "into sessions",
        "update sessions",
    ];

    /// This table's SQL-location guard: the walk and the matcher are shared with every other
    /// repository that has one (`crate::sql::guard`), so the two cannot drift or collide.
    fn guard() -> Guard {
        Guard::new("sessions", REPOSITORY, &STATEMENTS)
    }

    #[test]
    fn every_status_round_trips_through_its_text() {
        for status in Status::ALL {
            let parsed: Status = status
                .as_str()
                .parse()
                .expect("a status's own text parses back");
            assert_eq!(parsed, status);
            assert_eq!(status.to_string(), status.as_str());
        }
        let texts: Vec<&str> = Status::ALL.iter().map(|status| status.as_str()).collect();
        assert_eq!(
            texts,
            [
                "starting", "idle", "running", "stopped", "error", "deleting"
            ]
        );
    }

    #[test]
    fn the_duplicate_key_mapping_only_fires_on_a_unique_violation() {
        // `ffi::Error` is constructible, so the mapping is proved against the codes rather than
        // against a SQLite version's message text.
        let unique = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ffi::ErrorCode::ConstraintViolation,
                extended_code: rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE,
            },
            Some("UNIQUE constraint failed: sessions.client_key".to_string()),
        );
        assert!(is_duplicate_key(&unique), "{unique}");

        // 1299 is `SQLITE_CONSTRAINT_NOTNULL`: the same primary code, a different failure.
        let not_null = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ffi::ErrorCode::ConstraintViolation,
                extended_code: 1299,
            },
            Some("NOT NULL constraint failed: sessions.project".to_string()),
        );
        assert!(!is_duplicate_key(&not_null), "{not_null}");
        assert!(!is_duplicate_key(&rusqlite::Error::ExecuteReturnedResults));
    }

    #[test]
    fn a_status_that_is_not_one_of_the_six_is_refused_with_its_text() {
        // `ended` was removed from the spec's set, so it is exactly the kind of value an old row
        // could still hold.
        let error = "ended"
            .parse::<Status>()
            .expect_err("ended is not a status");
        assert!(matches!(error, RepoError::Status(ref text) if text == "ended"));
        assert!(
            error.to_string().contains("holds the status \"ended\""),
            "{error}"
        );
    }

    #[test]
    fn no_sessions_sql_outside_the_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let guard = guard();
        assert!(
            crate::sql::guard::sources_under(&root)
                .iter()
                .any(|(file, _)| file == REPOSITORY),
            "the walk did not find {root:?}/{REPOSITORY}"
        );
        let offenders = guard.offenders_under(&root);
        assert!(
            offenders.is_empty(),
            "a sessions statement lives outside src/{REPOSITORY}, which owns every read and \
             write of the table: {offenders:?}"
        );
    }

    #[test]
    fn the_guard_fires_on_a_planted_statement() {
        // The guard above passes over today's tree, so this one plants a tree where it must
        // fire: empty the walk's collection and this fails instead of the rule quietly
        // stopping. The other files are the shapes the matcher has to leave alone.
        let planted = guard().temp_dir("planted");
        planted.plant("planted.rs", "let sql = \"SELECT id FROM sessions\";\n");
        planted.plant("harmless.rs", "let sessions = 1;\n");
        planted.plant("suffix.rs", "let sql = \"SELECT * FROM sessions_notes\";\n");
        // A `\`-continuation after no space is not a break in Rust either: the two lines are the
        // one token `FROMsessions` at runtime, which is not a statement.
        planted.plant(
            "glued.rs",
            "let sql = \"SELECT id FROM\\\n     sessions\";\n",
        );
        planted.plant(
            "rename.rs",
            "let sql = \"ALTER TABLE old_sessions RENAME TO sessions\";\n",
        );
        planted.plant(
            "index.rs",
            "let sql = \"CREATE INDEX sessions_status ON sessions (status)\";\n",
        );
        assert_eq!(
            guard().offenders_under(planted.path()),
            ["planted.rs:1: from sessions"]
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
                "let sql = \"SELECT id FROM main.sessions\";\n",
                "prefix.rs:1: from sessions",
            ),
            (
                "quoted.rs",
                "let sql = \"SELECT id FROM \\\"sessions\\\"\";\n",
                "quoted.rs:1: from sessions",
            ),
            (
                "bracketed.rs",
                "let sql = \"DELETE FROM [sessions]\";\n",
                "bracketed.rs:1: delete from sessions",
            ),
            (
                "conflict.rs",
                "let sql = \"UPDATE OR REPLACE sessions SET status = 'idle'\";\n",
                "conflict.rs:1: update sessions",
            ),
            (
                "continued.rs",
                "let sql = \"SELECT id FROM \\\n     sessions\";\n",
                "continued.rs:1: from sessions",
            ),
        ] {
            let planted = guard.temp_dir("spelling");
            planted.plant(file, text);
            assert_eq!(guard.offenders_under(planted.path()), [reported], "{file}");
        }
    }
}
