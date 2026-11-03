//! The database `serve` opens and the migrations it applies (HS-004, ADR-0009).
//!
//! Every test here drives the public path — `Database::open`, `apply_pending` and the real
//! binary — against a temporary file, because WAL cannot be proved on `:memory:`.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use rusqlite::Connection;

use holosmith::db::{self, Database, DbError, OpenOptions};
use holosmith::schema::{self, EmbedError, Migration, SCHEMA_VERSION};

mod support;
use support::TempDir;

/// The `applied_at` these tests write, so the value can be asserted rather than raced.
const NOW: i64 = 1_760_000_000;

/// Write a database by hand: the migrations record, with `version` already in it.
///
/// WAL is set here as well, so the file `Database::open` receives is the one a holosmith
/// database is: it is not converted, which is what lets a lock test isolate the migration
/// transaction instead of the pragma that would convert it.
fn seed(path: &Path, version: u32) {
    let conn = Connection::open(path).expect("the seeded database opens");
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;\n\
         CREATE TABLE schema_version (\n\
             version    INTEGER PRIMARY KEY,\n\
             name       TEXT NOT NULL,\n\
             applied_at INTEGER NOT NULL\n\
         );",
    )
    .expect("the schema_version table is creatable");
    conn.execute(
        "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![version, "seeded.sql", NOW],
    )
    .expect("the seeded row is written");
}

/// Every row of `schema_version`, in version order.
fn recorded(path: &Path) -> Vec<(u32, String, i64)> {
    let conn = Connection::open(path).expect("the database is readable");
    let mut statement = conn
        .prepare("SELECT version, name, applied_at FROM schema_version ORDER BY version")
        .expect("schema_version is readable");
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("the query runs")
        .collect::<Result<Vec<(u32, String, i64)>, _>>()
        .expect("every row reads")
}

fn pragma_string(path: &Path, name: &str) -> String {
    Connection::open(path)
        .expect("the database is readable")
        .pragma_query_value(None, name, |row| row.get(0))
        .expect("the pragma answers")
}

#[test]
fn an_embed_that_disagrees_with_the_constant_refuses_before_the_file_exists() {
    // Guard 3: `Database::open` refuses a stale embed, which is what makes `SCHEMA_VERSION`
    // true of the artifact and not only of `cargo test` (ADR-0009). A literal slice proves the
    // refusal fires; deleting the `check_embed` call would leave every other test green.
    let dir = TempDir::new("stale-embed");
    let path = dir.database();
    // One migration past the constant, so this keeps working when the schema grows. Every
    // literal migration creates the registry, so a set that ever ran would also fail loudly —
    // but the assertion below is that `open` refused before the file was even created.
    let migrations: Vec<Migration> = (1..=SCHEMA_VERSION + 1)
        .map(|number| Migration {
            number,
            name: "literal",
            sql: "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);",
        })
        .collect();
    let error = Database::open_with(&path, OpenOptions::default(), NOW, &migrations)
        .expect_err("a migration past the constant cannot be opened");
    assert!(
        matches!(
            error,
            DbError::Embed(EmbedError::Disagrees { embedded, constant })
                if embedded == SCHEMA_VERSION + 1 && constant == SCHEMA_VERSION
        ),
        "expected a disagreement, got {error}"
    );
    assert!(!path.exists(), "guard 3 created {} anyway", path.display());
}

#[test]
fn a_write_lock_held_while_the_pragmas_run_is_the_same_busy_error() {
    // The measured lock-taking step is the WAL conversion, and it runs before the migration
    // transaction: a second start that hits the lock there must say what is happening, not
    // surface a bare "database is locked" (ADR-0009).
    let dir = TempDir::new("busy-pragma");
    let path = dir.database();
    let holder = Connection::open(&path).expect("the holder opens");
    holder
        .execute_batch("BEGIN IMMEDIATE")
        .expect("the holder takes the write lock");

    let error = Database::open(
        &path,
        OpenOptions {
            busy_timeout: Duration::ZERO,
        },
        NOW,
    )
    .expect_err("the pragma phase cannot convert a database another connection holds");
    assert!(matches!(error, DbError::Busy { .. }), "got {error}");
    assert!(
        error
            .to_string()
            .contains("another process holds the database's write lock"),
        "{error}"
    );
}

#[test]
fn the_first_start_creates_wal_and_records_the_migration() {
    let dir = TempDir::new("first-start");
    let path = dir.database();
    let database =
        Database::open(&path, OpenOptions::default(), NOW).expect("a fresh database opens");
    assert_eq!(database.path(), path);
    assert_eq!(database.schema_version(), SCHEMA_VERSION);
    assert!(path.is_file(), "app.db was not created");
    // `journal_mode` is the one pragma that outlives the connection, so a second connection is
    // the honest proof that the file itself is in WAL.
    assert_eq!(pragma_string(&path, "journal_mode"), "wal");
    assert_eq!(
        recorded(&path),
        vec![
            (1, "0001_initial.sql".to_string(), NOW),
            (2, "0002_sessions.sql".to_string(), NOW),
            (3, "0003_events.sql".to_string(), NOW),
        ],
        "the first start did not record every migration"
    );
}

#[test]
fn a_second_start_applies_nothing_and_records_nothing() {
    let dir = TempDir::new("second-start");
    let path = dir.database();
    drop(Database::open(&path, OpenOptions::default(), NOW).expect("the first start opens"));
    let second =
        Database::open(&path, OpenOptions::default(), NOW + 60).expect("a current database opens");
    assert_eq!(second.schema_version(), SCHEMA_VERSION);
    assert_eq!(
        recorded(&path),
        vec![
            (1, "0001_initial.sql".to_string(), NOW),
            (2, "0002_sessions.sql".to_string(), NOW),
            (3, "0003_events.sql".to_string(), NOW),
        ],
        "the second start rewrote the applied set"
    );
}

#[test]
fn a_newer_database_is_reported_and_not_migrated() {
    // Item 10: switching `current` back to an older release must not need a restore, so an
    // older binary warns and continues instead of refusing to start.
    let dir = TempDir::new("newer");
    let path = dir.database();
    seed(&path, SCHEMA_VERSION + 1);
    let database = Database::open(&path, OpenOptions::default(), NOW)
        .expect("a newer database opens on an older binary");
    assert_eq!(database.schema_version(), SCHEMA_VERSION + 1);
    assert_eq!(
        recorded(&path),
        vec![(SCHEMA_VERSION + 1, "seeded.sql".to_string(), NOW)],
        "the older binary wrote to a newer database"
    );
}

#[test]
fn a_current_database_is_opened_while_another_connection_holds_the_write_lock() {
    // ADR-0009: nothing pending means no write lock at all, so a start on a current database is
    // a no-op rather than a queue. A zero busy timeout makes it a proof: there is no waiting.
    let dir = TempDir::new("no-write-lock");
    let path = dir.database();
    drop(Database::open(&path, OpenOptions::default(), NOW).expect("the first start opens"));

    let holder = Connection::open(&path).expect("the holder opens");
    holder
        .execute_batch("BEGIN IMMEDIATE")
        .expect("the holder takes the write lock");

    let database = Database::open(
        &path,
        OpenOptions {
            busy_timeout: Duration::ZERO,
        },
        NOW,
    )
    .expect("a current database opens without the write lock");
    assert_eq!(database.schema_version(), SCHEMA_VERSION);
}

#[test]
fn a_broken_migration_rolls_back_and_names_the_file_and_the_statement() {
    let dir = TempDir::new("broken");
    let path = dir.database();
    let mut conn = Connection::open(&path).expect("the database opens");
    let migrations = [
        Migration {
            number: 1,
            name: "good",
            // The first migration bootstraps the record table, as `0001_initial.sql` does.
            sql: "CREATE TABLE schema_version (\n\
                      version    INTEGER PRIMARY KEY,\n\
                      name       TEXT NOT NULL,\n\
                      applied_at INTEGER NOT NULL\n\
                  );\n\
                  CREATE TABLE good (id INTEGER);",
        },
        Migration {
            number: 2,
            name: "broken",
            sql: "CREATE TABLE broken (id INTEGER);\n\
                  CREATE INDEX broken_by_id ON missing (id);\n",
        },
    ];
    let error = db::apply_pending(&mut conn, &path, &migrations, OpenOptions::default(), NOW)
        .expect_err("the second migration cannot apply");
    assert!(
        matches!(error, DbError::Migration { .. }),
        "expected a migration error, got {error}"
    );
    let message = error.to_string();
    assert!(
        message.contains("migration 0002_broken.sql failed:"),
        "{message}"
    );
    assert!(
        message.contains("statement: CREATE INDEX broken_by_id ON missing (id)"),
        "the failing statement is not named: {message}"
    );
    // One transaction for the whole batch: the first migration rolled back with the second.
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' \
             AND name IN ('schema_version', 'good', 'broken')",
            [],
            |row| row.get(0),
        )
        .expect("sqlite_master is readable");
    assert_eq!(tables, 0, "a failed migration left a table behind");
}

/// Start a `serve` process that opens `data_root`, and wait until it says where it listens.
///
/// The startup lines are the banner `serve` writes directly, so waiting for `control_listen`
/// is waiting for the database to have been opened and migrated.
fn start_serve(dir: &TempDir, name: &str, data_root: &Path) -> (std::process::Child, String) {
    let config = dir.file(&format!("{name}.toml"));
    fs::write(
        &config,
        format!(
            "control_listen = \"127.0.0.1:0\"\npreview_listen = \"127.0.0.1:8081\"\n\
             data_root = \"{}\"\n",
            data_root.display()
        ),
    )
    .expect("the config is writable");
    let stderr = dir.file(&format!("{name}.err"));
    let child = Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args([
            "serve",
            "--config",
            config.to_str().expect("the temp path is UTF-8"),
            "--port",
            "0",
            "--backend",
            "stub",
        ])
        .env_remove("HOLOSMITH_LOG")
        .stdout(std::process::Stdio::null())
        .stderr(fs::File::create(&stderr).expect("the log file is creatable"))
        .spawn()
        .expect("the binary starts");
    let deadline = std::time::Instant::now() + Duration::from_secs(40);
    let mut said = String::new();
    while std::time::Instant::now() < deadline {
        said = fs::read_to_string(&stderr).unwrap_or_default();
        if said.contains("control_listen ") {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    (child, said)
}

#[test]
fn two_processes_starting_at_once_end_at_one_schema() {
    // The acceptance says two *processes*, and it is also the only shape in which the wait is
    // real: SQLite answers `database is locked` without waiting when the lock is held by another
    // connection in the same process (measured), so two threads cannot prove this. Two
    // processes meet SQLite's write lock, wait on the busy timeout, and one of them finds
    // nothing left to apply.
    let dir = TempDir::new("concurrent-processes");
    let data_root = dir.file("data");
    let started = [
        start_serve(&dir, "first", &data_root),
        start_serve(&dir, "second", &data_root),
    ];
    for (mut child, said) in started {
        let _ = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
        let _ = child.wait();
        // Both printed the database they opened and the schema they recorded.
        let line = said
            .lines()
            .find(|line| line.starts_with("database "))
            .unwrap_or_else(|| panic!("a start printed no database line: {said}"));
        assert_eq!(
            line,
            format!(
                "database {} schema {SCHEMA_VERSION}",
                data_root.join(db::DATABASE_FILE).display()
            ),
            "{said}"
        );
    }
    let rows = recorded(&data_root.join(db::DATABASE_FILE));
    let applied: Vec<(u32, &str)> = rows
        .iter()
        .map(|(version, name, _)| (*version, name.as_str()))
        .collect();
    assert_eq!(
        applied,
        [
            (1, "0001_initial.sql"),
            (2, "0002_sessions.sql"),
            (3, "0003_events.sql"),
        ],
        "the migrations were not applied exactly once: {rows:?}"
    );
}

#[test]
fn a_held_write_lock_times_out_naming_the_path_and_the_timeout() {
    let dir = TempDir::new("busy");
    let path = dir.database();
    // Version 0 makes `0001_initial.sql` pending, so the start needs the write lock.
    seed(&path, 0);
    let holder = Connection::open(&path).expect("the holder opens");
    holder
        .execute_batch("BEGIN IMMEDIATE")
        .expect("the holder takes the write lock");

    let error = Database::open(
        &path,
        OpenOptions {
            busy_timeout: Duration::ZERO,
        },
        NOW,
    )
    .expect_err("the start cannot migrate past a held lock");
    assert!(matches!(error, DbError::Busy { .. }), "got {error}");
    let message = error.to_string();
    assert!(
        message.starts_with(&format!("cannot open {}: ", path.display())),
        "{message}"
    );
    assert!(
        message.contains("another process holds the database's write lock"),
        "{message}"
    );
    assert!(message.contains("gave up after"), "{message}");
}

#[test]
fn an_older_binary_on_a_newer_database_warns_and_continues() {
    // Item 10: the warning is half the promise, so it is asserted on the shipped path rather
    // than only "the start succeeded and wrote nothing" (the test above proves that half).
    let dir = TempDir::new("newer-warning");
    let path = dir.database();
    seed(&path, SCHEMA_VERSION + 1);
    let config = dir.file("config.toml");
    fs::write(&config, "control_listen = \"127.0.0.1:0\"\n").expect("the config is writable");
    let stderr = dir.file("serve.err");
    let mut child = Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args([
            "serve",
            "--config",
            config.to_str().expect("the temp path is UTF-8"),
            "--data-dir",
            dir.path().to_str().expect("the temp path is UTF-8"),
            "--port",
            "0",
            "--backend",
            "stub",
        ])
        // The default filter, which is what an operator gets: the warning has to survive it.
        .env_remove("HOLOSMITH_LOG")
        .stdout(std::process::Stdio::null())
        .stderr(fs::File::create(&stderr).expect("the log file is creatable"))
        .spawn()
        .expect("the binary starts");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut said = String::new();
    while std::time::Instant::now() < deadline {
        said = fs::read_to_string(&stderr).unwrap_or_default();
        if said.contains("control_listen ") {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let _ = child.wait();

    assert!(
        said.contains("was migrated by a newer release; continuing without migrating it"),
        "the start did not warn: {said}"
    );
    assert!(
        said.contains(&format!(
            "database {} schema {}",
            path.display(),
            SCHEMA_VERSION + 1
        )),
        "the start did not continue on the newer schema: {said}"
    );
    assert_eq!(
        recorded(&path),
        vec![(SCHEMA_VERSION + 1, "seeded.sql".to_string(), NOW)],
        "the older binary wrote to a newer database"
    );
}

#[test]
fn the_binary_exits_one_and_says_which_migration_statement_failed() {
    let dir = TempDir::new("binary-broken");
    let path = dir.database();
    // Version 0 with a correctly shaped table: the real `0001_initial.sql` collides with the
    // table it creates, so the real embedded file fails against a real database.
    seed(&path, 0);
    let config = dir.file("config.toml");
    fs::write(&config, "control_listen = \"127.0.0.1:0\"\n").expect("the config is writable");
    let output = Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args([
            "serve",
            "--config",
            config.to_str().expect("the temp path is UTF-8"),
            "--data-dir",
            dir.path().to_str().expect("the temp path is UTF-8"),
            "--port",
            "0",
            "--backend",
            "stub",
        ])
        .env_remove("HOLOSMITH_LOG")
        .output()
        .expect("the binary runs");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "holosmith: serve: migration 0001_initial.sql failed: table schema_version already exists"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("statement: CREATE TABLE schema_version"),
        "the failing statement is not named: {stderr}"
    );
    assert!(
        !stderr.contains("control_listen 127"),
        "a database that cannot be migrated must not bind: {stderr}"
    );
    // The pre-seeded table is exactly as it was: the rolled-back transaction changed nothing.
    assert_eq!(recorded(&path), vec![(0, "seeded.sql".to_string(), NOW)]);
}

#[test]
fn the_embedded_migrations_are_the_ones_the_constant_reports() {
    let migrations = schema::embedded().expect("the embedded migrations are well formed");
    let highest = migrations
        .last()
        .map(|migration| migration.number)
        .unwrap_or_default();
    assert_eq!(highest, SCHEMA_VERSION);
}
