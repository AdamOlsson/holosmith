//! The database the binary opens at startup, and the migrations it applies (HS-004, ADR-0009).
//!
//! `open` is the whole contract: the four pragmas the spec names are set in a fixed order and
//! read back, then every pending migration is applied in one transaction. The transaction is
//! also the startup lock — SQLite's own write lock, so a second process waits for the first
//! instead of applying the same migration twice — and a start on a current database takes no
//! write lock at all.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};

use crate::events::repository::EventRepository;
use crate::hosts::repository::HostRepository;
use crate::push::repository::PushRepository;
use crate::schema::{self, EmbedError, Migration};
use crate::sessions::repository::SessionRepository;
use crate::sql::split_statements;

/// The file under `data_root` that holds the schema (item 5).
pub const DATABASE_FILE: &str = "app.db";

/// How long a start waits for another process's migration before it gives up.
pub const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const SCHEMA_VERSION_EXISTS: &str =
    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'";
const RECORDED_VERSION: &str = "SELECT coalesce(max(version), 0) FROM schema_version";
const RECORD_APPLIED: &str =
    "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, ?3)";

/// What `open` may vary: only the busy timeout, so a test can use 100 ms instead of 5 s.
#[derive(Debug, Clone, Copy)]
pub struct OpenOptions {
    pub busy_timeout: Duration,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
        }
    }
}

/// An open, migrated database. Held for the process lifetime; the connection stays private, and a
/// handler reaches it only through the `Arc<Mutex<Database>>` ADR-0012 introduced and ADR-0013
/// keeps. HS-009 adds the single writer for events on top of that lock; a database actor that
/// funnels every read and write through one task is not part of it.
pub struct Database {
    /// The one connection this process has. `sessions` and `events` lend it to their repositories
    /// and nothing else reaches it; a handler gets to it only through the `Arc<Mutex<Database>>`
    /// that ADR-0012 introduced and ADR-0013 keeps.
    connection: Connection,
    path: PathBuf,
    schema_version: u32,
}

impl Database {
    /// Open (creating it if needed) and migrate the database at `path`.
    ///
    /// `now` is the `applied_at` written for each migration, in unix seconds, passed in so a
    /// test can assert the value rather than race the clock.
    pub fn open(path: &Path, options: OpenOptions, now: i64) -> Result<Self, DbError> {
        Self::open_with(
            path,
            options,
            now,
            &schema::embedded().map_err(DbError::Embed)?,
        )
    }

    /// `open` over an explicit migration set, so a test can prove guard 3 refuses before the
    /// file is touched. Public for the same reason `apply_pending` is: the tested path is the
    /// shipped path.
    pub fn open_with(
        path: &Path,
        options: OpenOptions,
        now: i64,
        migrations: &[Migration],
    ) -> Result<Self, DbError> {
        // Guard 3: the artifact's own migrations have to agree with the constant `version`
        // reports, because the naming and agreement tests only run under `cargo test`.
        schema::check_embed(migrations).map_err(DbError::Embed)?;
        let mut connection = Connection::open(path).map_err(sqlite_error_about(path))?;
        configured(&connection, path, options)?;
        let schema_version = apply_pending(&mut connection, path, migrations, options, now)?;
        Ok(Self {
            connection,
            path: path.to_path_buf(),
            schema_version,
        })
    }

    /// The file this database is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What `schema_version` records after the start: the reported schema.
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Every read and write of a `sessions` row (HS-007).
    ///
    /// The repository borrows this database's connection, so the connection itself stays private.
    /// A handler reaches it only through the `Arc<Mutex<Database>>` that ADR-0012 introduced and
    /// ADR-0013 keeps.
    pub fn sessions(&self) -> SessionRepository<'_> {
        SessionRepository::new(&self.connection)
    }

    /// Every read and write of an `events` row (HS-009).
    ///
    /// Like [`Self::sessions`], the repository borrows this database's connection rather than
    /// owning one. The writer thread is the only caller in production: it takes the lock, runs
    /// one `insert_batch`, and releases it before broadcasting (ADR-0013).
    pub fn events(&self) -> EventRepository<'_> {
        EventRepository::new(&self.connection)
    }

    /// Every read and write of a `push_subscriptions` row (HS-066).
    ///
    /// The same borrow as [`Self::sessions`] and [`Self::events`], and the rule those two already keep: a
    /// subscription is read under the lock, the guard is dropped, and only then does anything leave the machine.
    /// A push service that takes seconds to answer must not hold up a request.
    pub fn push(&self) -> PushRepository<'_> {
        PushRepository::new(&self.connection)
    }

    /// Every read and write of a `hosts` row (HS-068, spec items 4 and 16).
    ///
    /// The same borrow as the three above. The registry's *probe* is the one thing that must not happen under this
    /// lock: a health check leaves the machine and can take seconds. `http::hosts::register` probes inside
    /// `spawn_blocking` **before** it takes the guard, and `hosts::merge` is called by the boot on the plain
    /// `Database`, before the `Mutex` exists — so nothing in this build holds the guard across a probe. A caller
    /// that has only a `&Database` can still be holding one, which is why that rule is the caller's to keep.
    pub fn hosts(&self) -> HostRepository<'_> {
        HostRepository::new(&self.connection)
    }
}

impl fmt::Debug for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.path)
            .field("schema_version", &self.schema_version)
            .finish_non_exhaustive()
    }
}

/// An open, **read-only** connection: what a report reads a database with, so that reading a machine cannot
/// migrate it (HS-045's rule, HS-050's second reader).
///
/// [`Database::open`] applies every pending migration before it answers, which is what a *start* wants and what a
/// command that only describes a machine must not do: `doctor` refuses a database from a newer release rather than
/// touching it (ADR-0047), and `status` counts rows on a machine it was not asked to change. One opener for the
/// claim, so the two readers cannot disagree about what "read-only" means — SQLite's own flag, not a second
/// reading of the migration directory.
///
/// A read-only open of a WAL database needs to be able to create or map its `-shm` file, which is the data root's
/// own directory: both callers run as the user that owns it, and a file it cannot read is the caller's finding to
/// make, not a silent absence.
pub struct Reader {
    connection: Connection,
}

impl Reader {
    /// Open `path` read-only, or `None` when there is no file yet — a machine that has never served, which is not
    /// a finding (its own command says so in its own words).
    pub fn open(path: &Path) -> Result<Option<Self>, DbError> {
        // `NotFound` is the only "there is nothing to read". `Path::exists()` answers `false` for **any** stat
        // failure, which would turn "this user may not look inside the data root" into "this machine has never
        // served" — and one caller of this opener decides whether to delete the tree over that answer (HS-050).
        match std::fs::metadata(path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(DbError::Sqlite {
                    path: path.to_path_buf(),
                    source: rusqlite::Error::ToSqlConversionFailure(Box::new(error)),
                });
            }
        }
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(sqlite_error_about(path))?;
        Ok(Some(Self { connection }))
    }

    /// Every read of a `sessions` row, borrowed from this connection.
    pub fn sessions(&self) -> SessionRepository<'_> {
        SessionRepository::new(&self.connection)
    }

    /// Every read of a `hosts` row, borrowed from this connection.
    ///
    /// HS-072's `holosmith status` asks the registry a question a running `serve` writes: the facts are the last
    /// boot's probe (ADR-0073), so a read-only open answers them without a lock, a second writer or a server. This is
    /// the same repository the routes write through, so a row's columns are read in one place and a column a later
    /// migration adds cannot be missed here.
    pub fn hosts(&self) -> crate::hosts::repository::HostRepository<'_> {
        crate::hosts::repository::HostRepository::new(&self.connection)
    }

    /// The schema version the file records, for a caller that wants the same answer `doctor` makes a verdict of.
    pub fn schema_version(&self, path: &Path) -> Result<u32, DbError> {
        recorded_version(&self.connection, path)
    }

    /// Write this database's bytes to `destination` with `VACUUM INTO`, over the read-only connection (HS-078,
    /// spec item 10 step 4).
    ///
    /// One statement rather than a file copy for the reason item 10 names it: a copy of a live WAL database is
    /// three files that may not agree, while `VACUUM INTO` is the engine writing a consistent, compacted database
    /// out of a read transaction — and a **read-only** connection may run it (measured: SQLite allows it, and the
    /// destination is a new file the caller's uid can create). `upgrade` must not open this database read-write,
    /// because that open would migrate it before the candidate ever ran (ADR-0047).
    ///
    /// `VACUUM INTO` refuses to write over a file that exists, which is why the caller hands it a name that is not
    /// there and renames afterwards (and why the caller owns the rotation: this function knows one file).
    pub fn snapshot(&self, destination: &Path) -> Result<(), DbError> {
        let path = destination.display().to_string();
        self.connection
            .execute("VACUUM INTO ?1", [&path])
            .map(drop)
            .map_err(|error| DbError::Snapshot {
                destination: destination.to_path_buf(),
                source: error,
            })
    }
}

/// Why the database could not be opened, or could not be migrated.
#[derive(Debug)]
pub enum DbError {
    /// A pragma the spec requires is not in force.
    Pragma {
        path: PathBuf,
        pragma: &'static str,
        want: String,
        got: String,
    },
    /// A migration statement failed; the file and the statement are named.
    Migration {
        file: String,
        statement: String,
        source: rusqlite::Error,
    },
    /// Another process held the write lock longer than the busy timeout.
    Busy { path: PathBuf, timeout: Duration },
    /// SQLite refused a statement of our own.
    Sqlite {
        path: PathBuf,
        source: rusqlite::Error,
    },
    /// `VACUUM INTO` could not write the snapshot a promotion takes (HS-078).
    Snapshot {
        destination: PathBuf,
        source: rusqlite::Error,
    },
    /// The embedded migrations cannot be used.
    Embed(EmbedError),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pragma {
                path,
                pragma,
                want,
                got,
            } => write!(
                f,
                "cannot set {pragma}={want} on {}: it reports \"{got}\"",
                path.display()
            ),
            Self::Migration {
                file,
                statement,
                source,
            } => write!(
                f,
                "migration {file} failed: {source}\n  statement: {statement}"
            ),
            Self::Busy { path, timeout } => write!(
                f,
                "cannot open {}: another process holds the database's write lock (database is \
                 locked; gave up after {timeout:?})",
                path.display()
            ),
            Self::Sqlite { path, source } => write!(f, "cannot use {}: {source}", path.display()),
            Self::Snapshot {
                destination,
                source,
            } => write!(
                f,
                "the database could not be written to {} with `VACUUM INTO`: {source}",
                destination.display()
            ),
            Self::Embed(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Migration { source, .. }
            | Self::Sqlite { source, .. }
            | Self::Snapshot { source, .. } => Some(source),
            Self::Embed(error) => Some(error),
            Self::Pragma { .. } | Self::Busy { .. } => None,
        }
    }
}

/// The `DbError` for a `rusqlite` failure that is neither a lock nor a migration: it names the
/// file it is about.
fn sqlite_error_about(path: &Path) -> impl Fn(rusqlite::Error) -> DbError + '_ {
    move |error| DbError::Sqlite {
        path: path.to_path_buf(),
        source: error,
    }
}

/// A lock is SQLite's own write lock, and the waiter's timeout is the whole story: both the
/// pragma phase and the migration transaction report it as one error, so a second start says
/// what is happening instead of a bare "database is locked" (ADR-0009).
fn busy_or(path: &Path, timeout: Duration, error: rusqlite::Error) -> DbError {
    if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy) {
        return DbError::Busy {
            path: path.to_path_buf(),
            timeout,
        };
    }
    sqlite_error_about(path)(error)
}

/// The measured order, before any transaction: `busy_timeout`, `journal_mode=WAL`,
/// `synchronous=FULL`, `foreign_keys=ON`, then all four read back.
///
/// The WAL conversion comes after the busy timeout because it may need a lock, and it comes
/// before every `BEGIN` because SQLite refuses WAL from inside a transaction without an error.
/// The read-backs are the only way to know the spec's durability settings are actually in
/// force: a filesystem that cannot do WAL answers `"delete"` the same way.
///
/// What each read-back proves is not the same, and the difference is worth stating: `journal_mode`
/// is a property of the file, so `"wal"` is proof the database is in WAL. `synchronous` is
/// per-connection and SQLite's default is already `FULL(2)`, so reading `2` proves the statement
/// ran without an error and the value is now what we asked for — not that SQLite would otherwise
/// have answered anything different. `the_four_pragmas_read_back_on_a_real_file` sets it to `OFF`
/// first for exactly that reason, so its assertion reflects the statement rather than a default.
fn configured(conn: &Connection, path: &Path, options: OpenOptions) -> Result<(), DbError> {
    let failed = |error| busy_or(path, options.busy_timeout, error);
    conn.busy_timeout(options.busy_timeout).map_err(failed)?;
    let journal_mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(failed)?;
    read_back(path, "journal_mode", "WAL", &journal_mode)?;
    conn.pragma_update(None, "synchronous", "FULL")
        .map_err(sqlite_error_about(path))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(sqlite_error_about(path))?;
    // Read back · but read this one for what it is: `synchronous` is per-connection and SQLite's
    // default is already FULL(2), so a `2` here proves the statement ran without an error and
    // the value is now what was asked for. Only `journal_mode` above is proof of a property of
    // the file; the test that makes *this* assertion falsifiable sets OFF first (see above).
    let synchronous: i64 = conn
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .map_err(sqlite_error_about(path))?;
    read_back(path, "synchronous", "2", &synchronous.to_string())?;
    let foreign_keys: i64 = conn
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .map_err(sqlite_error_about(path))?;
    read_back(path, "foreign_keys", "1", &foreign_keys.to_string())?;
    let busy_timeout: i64 = conn
        .pragma_query_value(None, "busy_timeout", |row| row.get(0))
        .map_err(sqlite_error_about(path))?;
    read_back(
        path,
        "busy_timeout",
        &options.busy_timeout.as_millis().to_string(),
        &busy_timeout.to_string(),
    )
}

/// Fail the start, naming the pragma and the value it received.
fn read_back(path: &Path, pragma: &'static str, want: &str, got: &str) -> Result<(), DbError> {
    if got.eq_ignore_ascii_case(want) {
        return Ok(());
    }
    Err(DbError::Pragma {
        path: path.to_path_buf(),
        pragma,
        want: want.to_string(),
        got: got.to_string(),
    })
}

/// Apply every migration the database does not record yet, in **one** transaction.
///
/// Public because the runner takes `&[Migration]`: `Database::open` passes the embedded set and
/// a test can pass a literal slice, so the tested path is the shipped path (ADR-0009).
///
/// Nothing pending means no write transaction at all — that is what makes a second start a
/// no-op rather than a queue. Something pending means `BEGIN IMMEDIATE` and a re-read of
/// `max(version)` *inside* the transaction, so a process that waited for another process's
/// migration finds nothing left to do instead of applying it twice.
pub fn apply_pending(
    conn: &mut Connection,
    path: &Path,
    migrations: &[Migration],
    options: OpenOptions,
    now: i64,
) -> Result<u32, DbError> {
    let recorded = recorded_version(conn, path)?;
    // One decision, in `schema::verdict`, because the banner, the `warn` below and `doctor` (HS-045) all report the
    // same fact and a second comparison is a second chance to disagree (HS-075).
    match schema::verdict(recorded, schema::SCHEMA_VERSION) {
        // Item 10: rolling `current` back to an older binary must not need a restore, and expand/contract is what
        // makes continuing safe. `doctor` is where this refuses; `serve` says so once and carries on.
        schema::Verdict::Newer => {
            tracing::warn!(
                database = %path.display(),
                recorded,
                binary = schema::SCHEMA_VERSION,
                verdict = %schema::Verdict::Newer.sentence(recorded, schema::SCHEMA_VERSION),
                "a newer release wrote this database; continuing without migrating it"
            );
            return Ok(recorded);
        }
        // Nothing to do: the recorded version is the one this binary embeds.
        schema::Verdict::Same => return Ok(recorded),
        schema::Verdict::Older => {}
    }
    if !migrations
        .iter()
        .any(|migration| migration.number > recorded)
    {
        return Ok(recorded);
    }
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| busy_or(path, options.busy_timeout, error))?;
    // Re-read inside the transaction: the version read above is not locked.
    let recorded = recorded_version(&transaction, path)?;
    for migration in migrations
        .iter()
        .filter(|migration| migration.number > recorded)
    {
        for statement in split_statements(migration.sql) {
            transaction
                .execute(statement, [])
                .map_err(|error| DbError::Migration {
                    file: migration.file(),
                    statement: statement.to_string(),
                    source: error,
                })?;
        }
        transaction
            .execute(
                RECORD_APPLIED,
                params![migration.number, migration.file(), now],
            )
            .map_err(|error| DbError::Migration {
                file: migration.file(),
                statement: RECORD_APPLIED.to_string(),
                source: error,
            })?;
    }
    transaction.commit().map_err(sqlite_error_about(path))?;
    recorded_version(conn, path)
}

/// What `schema_version` records, or 0 when the table does not exist (an empty database).
fn recorded_version(conn: &Connection, path: &Path) -> Result<u32, DbError> {
    let tables: i64 = conn
        .query_row(SCHEMA_VERSION_EXISTS, [], |row| row.get(0))
        .map_err(sqlite_error_about(path))?;
    if tables == 0 {
        return Ok(0);
    }
    conn.query_row(RECORDED_VERSION, [], |row| row.get(0))
        .map_err(sqlite_error_about(path))
}

/// The schema version recorded in the database at `path`, read **without** migrating it (HS-045).
///
/// [`Database::open`] applies every pending migration, which is what a *start* wants and what `doctor` must not
/// do: ADR-0047's "a newer database" question has to be answered before anything writes to the file, and
/// `doctor --quick` runs as the unit file's `ExecStartPre`. So this opens it **read-only** — SQLite's own flag,
/// not a second reader of the migration directory — and runs the same two statements [`apply_pending`] decides
/// on. `None` is "there is no file yet", which is not a finding: a machine that has never served has no
/// database, and `serve` creates and migrates one at startup.
///
/// A read-only open of a WAL database needs to be able to create or map its `-shm` file, which is the data
/// root's own directory: `doctor` runs as the user that owns it, and a file it cannot read is the caller's
/// finding to make, not a silent `None`.
pub fn recorded_without_migrating(path: &Path) -> Result<Option<u32>, DbError> {
    match Reader::open(path)? {
        Some(reader) => reader.schema_version(path).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary database file that removes itself, so the tests leave nothing in `/tmp`.
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("holosmith-db-{label}-{}.db", std::process::id()));
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
            }
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }

    #[test]
    fn the_four_pragmas_read_back_on_a_real_file() {
        // WAL cannot be proved on `:memory:` (it answers "memory"), so every pragma assertion
        // needs a temporary file.
        let file = TempPath::new("pragmas");
        let conn = Connection::open(file.path()).expect("a temporary database opens");
        // `synchronous` needs a value to move *from* before a read-back of 2 is a proof: it is
        // per-connection and SQLite's default is already FULL(2), so `configured` doing nothing
        // would still leave 2 here. OFF first makes the assertion below falsifiable.
        conn.pragma_update(None, "synchronous", "OFF")
            .expect("synchronous is settable");
        let off: i64 = conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .expect("synchronous answers");
        assert_eq!(off, 0, "the scratch connection did not take OFF");
        configured(
            &conn,
            file.path(),
            OpenOptions {
                busy_timeout: Duration::from_millis(250),
            },
        )
        .expect("the pragmas are settable on a temporary file");
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("journal_mode answers");
        assert_eq!(journal_mode, "wal");
        let synchronous: i64 = conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .expect("synchronous answers");
        assert_eq!(
            synchronous, 2,
            "the read-back answered {synchronous} after it answered 0: the configured \
             synchronous did not take"
        );
        let foreign_keys: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .expect("foreign_keys answers");
        assert_eq!(foreign_keys, 1);
        let busy_timeout: i64 = conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .expect("busy_timeout answers");
        assert_eq!(busy_timeout, 250);
    }

    #[test]
    fn a_database_can_be_read_without_being_migrated() {
        // `doctor`'s reader (HS-045): the file is opened read-only and never written, so a start on a database
        // from a newer release can be refused instead of migrated (ADR-0047).
        let file = TempPath::new("recorded-without-migrating");
        assert_eq!(
            recorded_without_migrating(file.path()).expect("a missing file is not an error"),
            None,
            "a machine that never served has no database, which is not a finding"
        );
        let database = Database::open(file.path(), OpenOptions::default(), 1_760_000_000)
            .expect("the scratch database opens");
        assert_eq!(database.schema_version(), schema::SCHEMA_VERSION);
        drop(database);
        assert_eq!(
            recorded_without_migrating(file.path()).expect("the file is readable"),
            Some(schema::SCHEMA_VERSION)
        );

        // A version from the future is what `doctor` refuses on — and reading it must not rewrite the file,
        // which is the whole reason this is a second opener rather than `Database::open`.
        let conn = Connection::open(file.path()).expect("the scratch database reopens");
        conn.execute(RECORD_APPLIED, params![99, "from-the-future", 0])
            .expect("the future row is writable");
        drop(conn);
        assert_eq!(
            recorded_without_migrating(file.path()).expect("the file is readable"),
            Some(99)
        );
        assert_eq!(
            schema::verdict(99, schema::SCHEMA_VERSION),
            crate::schema::Verdict::Newer
        );
    }

    #[test]
    fn a_pragma_that_did_not_take_fails_the_start_naming_it() {
        // An in-memory database answers "memory" to `journal_mode=WAL`: the refusal is the
        // guard that the read-back exists for.
        let conn = Connection::open_in_memory().expect("an in-memory database opens");
        let error = configured(&conn, Path::new(":memory:"), OpenOptions::default())
            .expect_err("an in-memory database cannot be in WAL");
        assert!(
            matches!(
                error,
                DbError::Pragma {
                    pragma: "journal_mode",
                    ..
                }
            ),
            "expected a pragma error, got {error}"
        );
        assert_eq!(
            error.to_string(),
            "cannot set journal_mode=WAL on :memory:: it reports \"memory\""
        );
    }
}
