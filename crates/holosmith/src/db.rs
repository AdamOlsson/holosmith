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

use rusqlite::{Connection, TransactionBehavior, params};

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
/// handler reaches it only through ADR-0012's `Arc<Mutex<Database>>` stopgap until HS-009's single
/// writer owns the write path (ADR-0009, ADR-0012).
pub struct Database {
    /// The one connection this process has. `sessions` lends it to the repository and nothing else
    /// reaches it; a handler gets to it only through ADR-0012's `Arc<Mutex<Database>>` stopgap
    /// until HS-009's single writer owns the write path.
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
    /// A handler reaches it only through ADR-0012's `Arc<Mutex<Database>>` stopgap until HS-009's
    /// single writer owns the write path (ADR-0011, ADR-0012).
    pub fn sessions(&self) -> SessionRepository<'_> {
        SessionRepository::new(&self.connection)
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
            Self::Embed(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Migration { source, .. } | Self::Sqlite { source, .. } => Some(source),
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
    if recorded > schema::SCHEMA_VERSION {
        // Item 10: rolling `current` back to an older binary must not need a restore, and
        // expand/contract is what makes continuing safe.
        tracing::warn!(
            database = %path.display(),
            recorded,
            binary = schema::SCHEMA_VERSION,
            "the database was migrated by a newer release; continuing without migrating it"
        );
        return Ok(recorded);
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
        assert_eq!(synchronous, 2);
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
