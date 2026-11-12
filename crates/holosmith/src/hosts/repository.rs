//! Every statement that reads or writes a `hosts` row (spec items 4 and 16).
//!
//! The repository borrows the open `Database`'s connection rather than owning one, exactly as `sessions`, `events`
//! and `push_subscriptions` do, and this file is the only one with a `hosts` statement — a test below walks `src/**`
//! and fails if one appears anywhere else (`crate::sql::guard`, the shared matcher).
//!
//! Four statements and one rule. The rule is [`HostRepository::upsert`]'s: **a name is the identity**, so a
//! registration of a name that is already there replaces its URL, its reach and its error rather than adding a
//! second row. That is what makes `POST /api/hosts` idempotent, and it is what "the file wins on conflicts" means
//! one module over — the merge writes through this same statement.
//!
//! Nothing here probes anything. A probe leaves the machine and can take seconds, so every caller reads the rows,
//! drops the lock, probes, and takes it again to write what it measured.

use rusqlite::{Connection, Row, params};

use crate::config::Origin;
use crate::hosts::{Host, Reach};
use crate::sessions::RepoError;

/// The seven columns a row is, in the order every statement below reads them and [`host`] unpacks them.
///
/// `preview` is **last** because it is the column added (migration `0007`): appending it leaves every index
/// this file already used where it was, which is one fewer thing for a reader to re-derive from a diff.
const COLUMNS: &str = "name, url, reach, version, last_error, last_checked_at, preview";

/// The write: one row per name, and a second registration of one name is that machine registered again.
const UPSERT: &str = "INSERT INTO hosts (name, url, reach, version, last_error, last_checked_at, preview) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
     ON CONFLICT(name) DO UPDATE SET url = excluded.url, reach = excluded.reach, \
     version = excluded.version, last_error = excluded.last_error, \
     last_checked_at = excluded.last_checked_at, preview = excluded.preview";

/// The removal: what `DELETE /api/hosts/{name}` does once the refuse-if-live rule has its say.
const REMOVE: &str = "DELETE FROM hosts WHERE name = ?1";

/// One statement, read by name.
fn select_one() -> String {
    format!("SELECT {COLUMNS} FROM hosts WHERE name = ?1")
}

/// Every row, by name: one order, so two answers to one question are the same answer.
fn select_all() -> String {
    format!("SELECT {COLUMNS} FROM hosts ORDER BY name ASC")
}

/// The only way to read or write a `hosts` row. Obtained from `Database::hosts`.
pub struct HostRepository<'a> {
    connection: &'a Connection,
}

impl<'a> HostRepository<'a> {
    /// Borrow `connection`. `pub(crate)` rather than `pub`, so `Database::hosts` is the only way a caller gets one
    /// and the connection itself never leaves the database.
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    /// Store `host` and answer the row as it now stands.
    ///
    /// The answer is a `SELECT` rather than the value that was passed in, for the reason
    /// [`crate::push::repository`]'s upsert gives one table over: a caller that reported what it meant to write would
    /// be describing a row the table may not hold.
    pub fn upsert(&self, host: &Host) -> Result<Host, RepoError> {
        let sqlite = |error| RepoError::sqlite("upsert", error);
        self.connection
            .execute(
                UPSERT,
                params![
                    host.name,
                    host.url.as_str(),
                    host.reach.as_str(),
                    host.version,
                    host.last_error,
                    host.last_checked_at,
                    host.preview.as_ref().map(|preview| preview.as_str()),
                ],
            )
            .map_err(sqlite)?;
        self.connection
            .query_row(&select_one(), [&host.name], host_row)
            .map_err(sqlite)
    }

    /// One row by name; `None` when there is no such host.
    pub fn one(&self, name: &str) -> Result<Option<Host>, RepoError> {
        let mut statement = self
            .connection
            .prepare(&select_one())
            .map_err(|error| RepoError::sqlite("one", error))?;
        let mut rows = statement
            .query_map([name], host_row)
            .map_err(|error| RepoError::sqlite("one", error))?;
        match rows.next() {
            Some(row) => row
                .map(Some)
                .map_err(|error| RepoError::sqlite("one", error)),
            None => Ok(None),
        }
    }

    /// Every row, by name.
    pub fn all(&self) -> Result<Vec<Host>, RepoError> {
        let mut statement = self
            .connection
            .prepare(&select_all())
            .map_err(|error| RepoError::sqlite("all", error))?;
        let rows = statement
            .query_map([], host_row)
            .map_err(|error| RepoError::sqlite("all", error))?;
        rows.collect::<Result<Vec<Host>, _>>()
            .map_err(|error| RepoError::sqlite("all", error))
    }

    /// Forget one row: `Ok(false)` when there was no such host.
    ///
    /// `false` rather than an error, because the route has a sentence of its own for it (`404 not_found`) and a row
    /// that is already gone is not a failure of the database.
    pub fn remove(&self, name: &str) -> Result<bool, RepoError> {
        self.connection
            .execute(REMOVE, [name])
            .map(|changed| changed > 0)
            .map_err(|error| RepoError::sqlite("remove", error))
    }
}

/// One row, read in the order [`COLUMNS`] selects.
///
/// The two text columns that are words are parsed here rather than at every reader: a `reach` that is not one of
/// the two, or a `url` that is not an origin, is a corrupt row, and it is an error naming the text it found — ADR-0011's
/// rule for a status one table over. The parse's sentence becomes a SQLite conversion failure so it travels in the
/// same [`RepoError`] every other statement of this repository answers in.
fn host_row(row: &Row<'_>) -> rusqlite::Result<Host> {
    let name: String = row.get(0)?;
    let url: String = row.get(1)?;
    let url = Origin::parse(&url).map_err(|error| corrupt(1, error))?;
    let reach: String = row.get(2)?;
    let reach = Reach::parse(&reach).map_err(|error| corrupt(2, error))?;
    // A NULL is a row no probe has carried a preview origin for, which is not corruption; a text that is not an
    // origin is, and it names the column it was in (ADR-0074).
    let preview: Option<String> = row.get(6)?;
    let preview = match preview {
        Some(preview) => Some(Origin::parse(&preview).map_err(|error| corrupt(6, error))?),
        None => None,
    };
    Ok(Host {
        name,
        url,
        preview,
        reach,
        version: row.get(3)?,
        last_error: row.get(4)?,
        last_checked_at: row.get(5)?,
    })
}

/// A text column that is not what the row's own type says it is, as the error `query_row` wants.
fn corrupt(column: usize, sentence: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            sentence,
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, OpenOptions};
    use crate::sql::guard::Guard;

    /// The one file allowed to name the table in a statement.
    const REPOSITORY: &str = "hosts/repository.rs";

    /// The spellings a `hosts` statement can take, longest first so `delete from hosts` is reported as itself
    /// rather than as `from hosts`.
    const STATEMENTS: [&str; 4] = [
        "delete from hosts",
        "from hosts",
        "into hosts",
        "update hosts",
    ];

    /// This table's SQL-location guard: the walk and the matcher are shared with every other repository that has
    /// one (`crate::sql::guard`), so the two cannot drift or collide.
    fn guard() -> Guard {
        Guard::new("hosts", REPOSITORY, &STATEMENTS)
    }

    /// A database in a scratch directory that removes itself, and its path.
    fn database(label: &str) -> (std::path::PathBuf, Database) {
        let dir = std::env::temp_dir().join(format!(
            "holosmith-hosts-repository-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch directory is creatable");
        let path = dir.join("app.db");
        let database = Database::open(&path, OpenOptions::default(), 1_760_000_000)
            .expect("a fresh database opens");
        (dir, database)
    }

    fn host(name: &str, url: &str, reach: Reach) -> Host {
        Host {
            name: name.to_string(),
            url: Origin::parse(url).expect("the test's URL is an origin"),
            preview: Some(
                Origin::parse("http://localhost:8081").expect("the test's preview is an origin"),
            ),
            reach,
            version: Some("0.1.0".to_string()),
            last_error: None,
            last_checked_at: 1_760_000_001,
        }
    }

    #[test]
    fn every_hosts_statement_is_in_this_file() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let sources = crate::sql::guard::sources_under(&root);
        assert!(
            sources.len() > 40,
            "the walk found {} files, so it did not read this crate",
            sources.len()
        );
        let offenders = guard().offenders_under(&root);
        assert_eq!(
            offenders,
            Vec::<String>::new(),
            "a hosts statement lives outside {REPOSITORY}"
        );
    }

    #[test]
    fn the_guard_fires_on_a_planted_statement() {
        let planted = guard().temp_dir("planted");
        planted.plant("planted.rs", "let sql = \"SELECT name FROM hosts\";\n");
        planted.plant("harmless.rs", "let hosts = 1;\n");
        // A nearby word is not this table: the matcher is a word match, not a prefix one.
        planted.plant("suffix.rs", "let sql = \"SELECT * FROM hosts_notes\";\n");
        assert_eq!(
            guard().offenders_under(planted.path()),
            ["planted.rs:1: from hosts"]
        );
    }

    #[test]
    fn a_name_is_the_identity_and_the_second_write_wins() {
        let (dir, database) = database("upsert");
        let first = database
            .hosts()
            .upsert(&host(
                "workshop",
                "https://workshop.tail.ts.net",
                Reach::Reachable,
            ))
            .expect("the first write");
        assert_eq!(first.reach, Reach::Reachable);
        let mut second = host("workshop", "https://attic.tail.ts.net", Reach::Unreachable);
        second.last_error = Some("connection refused".to_string());
        second.version = None;
        let stored = database.hosts().upsert(&second).expect("the second write");
        assert_eq!(stored.url.as_str(), "https://attic.tail.ts.net");
        assert_eq!(stored.reach, Reach::Unreachable);
        assert_eq!(stored.last_error.as_deref(), Some("connection refused"));
        let all = database.hosts().all().expect("the table reads");
        assert_eq!(all.len(), 1, "one name is one row");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_list_is_by_name_and_the_removal_says_whether_it_removed_anything() {
        let (dir, database) = database("list");
        for name in ["workshop", "attic", "box"] {
            database
                .hosts()
                .upsert(&host(name, "https://box.tail.ts.net", Reach::Unreachable))
                .expect("the write");
        }
        let names: Vec<String> = database
            .hosts()
            .all()
            .expect("the table reads")
            .into_iter()
            .map(|host| host.name)
            .collect();
        assert_eq!(names, ["attic", "box", "workshop"]);
        assert!(database.hosts().remove("box").expect("the removal"));
        assert!(!database.hosts().remove("box").expect("the second removal"));
        assert_eq!(database.hosts().one("box").expect("the read"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_row_is_an_error_naming_what_it_found() {
        let (dir, database) = database("corrupt");
        database
            .hosts()
            .upsert(&host(
                "workshop",
                "https://workshop.tail.ts.net",
                Reach::Reachable,
            ))
            .expect("the write");
        // A row no writer of this build can make, so only the reader's own rule can refuse it. The test's own
        // connection, because this statement is the test's and not a repository method: it is *in* the one file the
        // guard allows to name the table.
        {
            let connection =
                rusqlite::Connection::open(dir.join("app.db")).expect("the database opens");
            connection
                .execute(
                    "UPDATE hosts SET reach = 'maybe' WHERE name = 'workshop'",
                    [],
                )
                .expect("the test writes the word");
        }
        let error = database
            .hosts()
            .all()
            .expect_err("a third word is not a reach");
        assert!(error.to_string().contains("maybe"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
