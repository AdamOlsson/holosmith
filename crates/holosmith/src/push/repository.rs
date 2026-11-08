//! Every statement that reads or writes the `push_subscriptions` table (HS-066, spec item 11).
//!
//! The repository borrows the open `Database`'s connection rather than owning one, exactly as `sessions` and
//! `events` do, and this file is the only one with a `push_subscriptions` statement. A test below walks `src/**`
//! and fails if one appears anywhere else, which is what makes "no SQL outside the repository" checkable rather
//! than a review note (`crate::sql::guard`, the shared matcher).
//!
//! Three statements and one rule. The rule is [`PushRepository::upsert`]'s: a browser re-subscribing hands in the
//! **same endpoint** with new keys, so the write is an upsert keyed on the endpoint and `created_at` keeps the
//! first value — the row's age is when this machine first heard from that device, not when it last did.

use rusqlite::{Connection, Row, params};

use crate::sessions::RepoError;

/// The write: one row per endpoint, and a second subscribe with the same endpoint is the browser's own
/// re-subscribe — the keys move and `created_at` does not.
///
/// `DO UPDATE SET` rather than `DO NOTHING` because the keys *are* what may have rotated: a subscription whose
/// `p256dh` an operator's browser replaced would otherwise never receive a readable message again, and the
/// failure would look like a dead device.
const UPSERT: &str = "INSERT INTO push_subscriptions (endpoint, p256dh, auth, created_at) \
     VALUES (?1, ?2, ?3, ?4) \
     ON CONFLICT(endpoint) DO UPDATE SET p256dh = excluded.p256dh, auth = excluded.auth";

/// One row, by its key.
const SELECT_ONE: &str = "SELECT endpoint, p256dh, auth, created_at FROM push_subscriptions \
     WHERE endpoint = ?1";

/// Every subscription, oldest first — the order a send walks them in.
///
/// `created_at` alone is not a total order (two devices can subscribe inside one second), and the endpoint after
/// it is, which is what makes a send's order one answer rather than two.
const SELECT_ALL: &str = "SELECT endpoint, p256dh, auth, created_at FROM push_subscriptions \
     ORDER BY created_at ASC, endpoint ASC";

/// The removal: what a push service's `404`/`410` earns (spec item 11's "dead endpoints are removed").
const REMOVE: &str = "DELETE FROM push_subscriptions WHERE endpoint = ?1";

/// One stored subscription, as the table holds it and as a send needs it.
///
/// `p256dh` and `auth` are the rows' own base64url text rather than decoded bytes: the API stores what a browser
/// sent, the send decodes once (`crate::vapid::unbase64url`), and a decoded copy in the row would be a second
/// spelling of one subscription that a hand-edited database could make disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub created_at: i64,
}

/// The only way to read or write a `push_subscriptions` row. Obtained from `Database::push`.
pub struct PushRepository<'a> {
    connection: &'a Connection,
}

impl<'a> PushRepository<'a> {
    /// Borrow `connection`. `pub(crate)` rather than `pub`, so `Database::push` is the only way a caller gets one
    /// and the connection itself never leaves the database.
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    /// Store `endpoint`'s subscription and answer the row as it now stands.
    ///
    /// The answer is a `SELECT` rather than an assembled value so it carries the **real** `created_at`: on a
    /// re-subscribe that is the row's first, not `now`, and a caller that reported `now` would be describing a
    /// row the table does not hold.
    pub fn upsert(
        &self,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
        now: i64,
    ) -> Result<Stored, RepoError> {
        let sqlite = |error| RepoError::sqlite("upsert", error);
        self.connection
            .execute(UPSERT, params![endpoint, p256dh, auth, now])
            .map_err(sqlite)?;
        self.connection
            .query_row(SELECT_ONE, [endpoint], stored)
            .map_err(sqlite)
    }

    /// Forget `endpoint`'s subscription: `Ok(false)` when there was no such row.
    ///
    /// `false` rather than an error, because both callers have a sentence of their own for it — the `DELETE`
    /// route answers `404`, and a send that found the row already gone says nothing at all — and neither is a
    /// failure of the database.
    pub fn remove(&self, endpoint: &str) -> Result<bool, RepoError> {
        self.connection
            .execute(REMOVE, [endpoint])
            .map(|changed| changed > 0)
            .map_err(|error| RepoError::sqlite("remove", error))
    }

    /// Every subscription, oldest first.
    pub fn all(&self) -> Result<Vec<Stored>, RepoError> {
        let mut statement = self
            .connection
            .prepare(SELECT_ALL)
            .map_err(|error| RepoError::sqlite("all", error))?;
        let rows = statement
            .query_map([], stored)
            .map_err(|error| RepoError::sqlite("all", error))?;
        rows.collect::<Result<Vec<Stored>, _>>()
            .map_err(|error| RepoError::sqlite("all", error))
    }
}

/// One row, read in the order both statements select: `endpoint, p256dh, auth, created_at`.
fn stored(row: &Row<'_>) -> rusqlite::Result<Stored> {
    Ok(Stored {
        endpoint: row.get(0)?,
        p256dh: row.get(1)?,
        auth: row.get(2)?,
        created_at: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, OpenOptions};
    use crate::sql::guard::Guard;

    /// The one file allowed to name the table in a statement.
    const REPOSITORY: &str = "push/repository.rs";

    /// The spellings a `push_subscriptions` statement can take, longest first so `delete from
    /// push_subscriptions` is reported as itself rather than as `from push_subscriptions`.
    const STATEMENTS: [&str; 4] = [
        "delete from push_subscriptions",
        "from push_subscriptions",
        "into push_subscriptions",
        "update push_subscriptions",
    ];

    /// This table's SQL-location guard: the walk and the matcher are shared with every other repository that has
    /// one (`crate::sql::guard`), so the two cannot drift or collide.
    fn guard() -> Guard {
        Guard::new("push_subscriptions", REPOSITORY, &STATEMENTS)
    }

    /// A database in a scratch directory that removes itself, and its path.
    ///
    /// `label` names the test, because two `Database::open` calls on one path in one process is ADR-0009's measured
    /// "database is locked" trap and these tests run in parallel; the table word is there for
    /// `crate::sql::guard`'s reason, and the process id so a second binary in one run cannot delete this one's live
    /// database.
    fn database(label: &str) -> (std::path::PathBuf, Database) {
        let dir = std::env::temp_dir().join(format!(
            "holosmith-push-repository-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch directory is creatable");
        let path = dir.join("app.db");
        let database = Database::open(&path, OpenOptions::default(), 1_760_000_000)
            .expect("a fresh database opens");
        (dir, database)
    }

    #[test]
    fn every_push_subscriptions_statement_is_in_this_file() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        // The guard passes over today's tree — and proves it read something, so a walk that found no files
        // cannot pass by being empty.
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
            "a push_subscriptions statement lives outside {REPOSITORY}"
        );
    }

    #[test]
    fn the_guard_fires_on_a_planted_statement() {
        let planted = guard().temp_dir("planted");
        planted.plant(
            "planted.rs",
            "let sql = \"SELECT endpoint FROM push_subscriptions\";\n",
        );
        planted.plant("harmless.rs", "let push = 1;\n");
        // A nearby word is not this table: the matcher is a word match, not a prefix one.
        planted.plant(
            "suffix.rs",
            "let sql = \"SELECT * FROM push_subscriptions_notes\";\n",
        );
        assert_eq!(
            guard().offenders_under(planted.path()),
            ["planted.rs:1: from push_subscriptions"]
        );
    }

    #[test]
    fn a_second_subscribe_moves_the_keys_and_keeps_the_created_at() {
        let (_dir, database) = database("resubscribe");
        let repository = database.push();
        let first = repository
            .upsert("https://push.example/one", "old-p256dh", "old-auth", 1_000)
            .expect("the first subscribe is stored");
        assert_eq!(
            first,
            Stored {
                endpoint: "https://push.example/one".to_string(),
                p256dh: "old-p256dh".to_string(),
                auth: "old-auth".to_string(),
                created_at: 1_000,
            }
        );
        // The browser re-subscribing: one row, the newer keys, and the original `created_at`.
        let again = repository
            .upsert("https://push.example/one", "new-p256dh", "new-auth", 2_000)
            .expect("a re-subscribe is stored");
        assert_eq!(again.p256dh, "new-p256dh");
        assert_eq!(again.auth, "new-auth");
        assert_eq!(again.created_at, 1_000, "the row's age moved with the keys");
        assert_eq!(repository.all().expect("the rows read").len(), 1);
    }

    #[test]
    fn all_answers_oldest_first_and_remove_answers_whether_there_was_a_row() {
        let (_dir, database) = database("order");
        let repository = database.push();
        for (endpoint, now) in [
            ("https://push.example/late", 2_000),
            ("https://push.example/early", 1_000),
        ] {
            repository
                .upsert(endpoint, "p256dh", "auth", now)
                .expect("the row is stored");
        }
        let endpoints: Vec<String> = repository
            .all()
            .expect("the rows read")
            .into_iter()
            .map(|stored| stored.endpoint)
            .collect();
        assert_eq!(
            endpoints,
            [
                "https://push.example/early".to_string(),
                "https://push.example/late".to_string()
            ]
        );
        assert!(
            repository
                .remove("https://push.example/early")
                .expect("the first removal runs"),
            "the stored row was not removed"
        );
        // The second removal is `false`, not an error: the row is already gone and that is the answer.
        assert!(
            !repository
                .remove("https://push.example/early")
                .expect("the second removal runs"),
            "a row that is already gone reported itself as removed"
        );
        assert_eq!(repository.all().expect("the rows read").len(), 1);
    }
}
