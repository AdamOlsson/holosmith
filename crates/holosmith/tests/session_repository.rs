//! The `sessions` row as the repository stores and returns it (HS-007).
//!
//! Every test drives `Database::sessions`, the path the shipped binary uses. Two of them reach
//! the file with `rusqlite` directly, and only for what the repository deliberately cannot do:
//! read the table's own schema, and write a status the `Status` enum refuses. That is why the
//! "no SQL outside the repository" guard walks `src/**` and not `tests/**`.

use std::path::Path;

use rusqlite::Connection;

use holosmith::sessions::{NewSession, RepoError, Status};

mod support;
use support::{TempDir, open};

/// The timestamps these tests write, so the value can be asserted rather than raced.
const NOW: i64 = 1_760_000_000;

/// The create body these tests share: the four fields HS-008 will validate, plus the key.
fn new_session(client_key: Option<&str>) -> NewSession {
    NewSession {
        project: "holosmith".to_string(),
        model: "claude".to_string(),
        thinking: "high".to_string(),
        network: false,
        client_key: client_key.map(str::to_string),
        base_commit: None,
    }
}

/// One row of `PRAGMA table_info(sessions)`: name, declared type, NOT NULL, default, primary key.
type Column = (String, String, bool, Option<String>, bool);

// ---- the migration -------------------------------------------------------------------

/// `PRAGMA table_info(sessions)`, read from the file the binary migrated.
fn table_info(path: &Path) -> Vec<Column> {
    let conn = Connection::open(path).expect("the database is readable");
    let mut statement = conn
        .prepare("PRAGMA table_info(sessions)")
        .expect("the pragma is preparable");
    statement
        .query_map([], |row| {
            Ok((
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get(4)?,
                row.get::<_, i64>(5)? != 0,
            ))
        })
        .expect("the pragma answers")
        .collect::<Result<Vec<Column>, _>>()
        .expect("every column reads")
}

#[test]
fn the_table_has_every_column_the_spec_names() {
    let dir = TempDir::new("columns");
    let database = open(&dir);
    drop(database);
    let columns = table_info(&dir.database());
    let names: Vec<&str> = columns.iter().map(|column| column.0.as_str()).collect();
    // The spec's fragment plus the columns the lifecycle needs and the spec implies.
    assert_eq!(
        names,
        [
            "id",
            "project",
            "model",
            "thinking",
            "worktree",
            "base_commit",
            "status",
            "status_reason",
            "client_key",
            "network",
            "created_at",
            "last_active_at",
            "stopped_at",
            "usage_in",
            "usage_out",
            "last_seq",
            "expected_stop",
        ],
        "the sessions table is not the one the spec names: {columns:?}"
    );

    let column = |name: &str| -> &Column {
        columns
            .iter()
            .find(|column| column.0 == name)
            .unwrap_or_else(|| panic!("sessions has no {name} column"))
    };
    let text = |name: &str| column(name).1.as_str();
    for name in [
        "id",
        "project",
        "model",
        "thinking",
        "worktree",
        "status",
        "client_key",
    ] {
        assert_eq!(text(name), "TEXT", "{name} is not TEXT");
    }
    for name in [
        "network",
        "created_at",
        "last_active_at",
        "usage_in",
        "usage_out",
        "last_seq",
        "expected_stop",
    ] {
        assert_eq!(text(name), "INTEGER", "{name} is not INTEGER");
    }
    // `worktree` is NOT NULL and holds '' until the provisioner runs (HS-007's note), rather
    // than becoming nullable; every other non-nullable column is here too, so dropping one
    // `NOT NULL` fails a test. `id` is the exception: SQLite cannot express `NOT NULL` on a
    // `TEXT PRIMARY KEY`, and that is recorded in ADR-0011 rather than defended.
    for name in [
        "project",
        "model",
        "thinking",
        "worktree",
        "status",
        "network",
        "created_at",
        "last_active_at",
        "usage_in",
        "usage_out",
        "last_seq",
        "expected_stop",
    ] {
        assert!(column(name).2, "{name} must be NOT NULL");
    }
    assert!(column("id").4, "id must be the primary key");
    // The four nullable columns.
    for name in ["base_commit", "status_reason", "client_key", "stopped_at"] {
        assert!(!column(name).2, "{name} must be nullable");
    }
    // The spec's three `DEFAULT 0` columns, `network` being the one the acceptance names, plus
    // HS-009's `last_seq`, which makes every pre-HS-009 row correct without a backfill.
    for name in ["network", "usage_in", "usage_out", "last_seq"] {
        assert_eq!(
            column(name).3.as_deref(),
            Some("0"),
            "{name} must default to 0"
        );
    }
    // The id is generated by the repository, not by a column default (ADR-0011).
    assert_eq!(column("id").3, None, "id must have no default");
}

#[test]
fn the_two_indexes_the_cap_and_eviction_need_are_there() {
    let dir = TempDir::new("indexes");
    drop(open(&dir));
    let conn = Connection::open(dir.database()).expect("the database is readable");
    let mut statement = conn
        .prepare("PRAGMA index_list(sessions)")
        .expect("the pragma is preparable");
    let indexes: Vec<(String, bool)> = statement
        .query_map([], |row| Ok((row.get(1)?, row.get::<_, i64>(2)? != 0)))
        .expect("the pragma answers")
        .collect::<Result<Vec<_>, _>>()
        .expect("every index reads");
    let named: Vec<&str> = indexes
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| !name.starts_with("sqlite_autoindex"))
        .collect();
    assert_eq!(named.len(), 2, "the sessions indexes are {indexes:?}");
    assert!(
        named.contains(&"sessions_status") && named.contains(&"sessions_last_active"),
        "{indexes:?}"
    );
    // Two of SQLite's own indexes are unique: the primary key's, because a `TEXT` key is not the
    // rowid, and `client_key UNIQUE`. Neither is a `CHECK` on `status`, which has none on purpose
    // (ADR-0011).
    let unique: Vec<&str> = indexes
        .iter()
        .filter(|(_, unique)| *unique)
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(unique.len(), 2, "the unique indexes are {indexes:?}");
    assert!(
        unique
            .iter()
            .all(|name| name.starts_with("sqlite_autoindex")),
        "{indexes:?}"
    );
}

// ---- create --------------------------------------------------------------------------

#[test]
fn a_new_session_is_starting_in_an_empty_worktree() {
    let dir = TempDir::new("create");
    let database = open(&dir);
    let created = database
        .sessions()
        .create(&new_session(Some("key-1")), NOW)
        .expect("a session is creatable");
    assert_eq!(created.status, Status::Starting);
    assert_eq!(
        created.worktree, "",
        "worktree is '' until the provisioner runs"
    );
    assert_eq!(created.base_commit, None);
    assert_eq!(created.status_reason, None);
    assert_eq!(created.client_key.as_deref(), Some("key-1"));
    assert!(
        !created.network,
        "network defaults to the checkbox being off"
    );
    assert_eq!(created.created_at, NOW);
    assert_eq!(created.last_active_at, NOW);
    assert_eq!(created.stopped_at, None);
    assert_eq!((created.usage_in, created.usage_out), (0, 0));
}

#[test]
fn base_commit_round_trips_when_the_create_carries_one() {
    // The only value-carrying `Option` the create writes (`?5`): `new_session` leaves it `None`,
    // so without this the binding and the read-back are unproved.
    let dir = TempDir::new("base-commit");
    let database = open(&dir);
    let repository = database.sessions();
    let mut new = new_session(Some("base-commit"));
    new.base_commit = Some("5f2c1d0e9b8a7c6d5e4f3a2b1c0d9e8f7a6b5c4d".to_string());
    let created = repository
        .create(&new, NOW)
        .expect("a session is creatable");
    assert_eq!(
        created.base_commit.as_deref(),
        Some("5f2c1d0e9b8a7c6d5e4f3a2b1c0d9e8f7a6b5c4d")
    );
    assert_eq!(
        repository.get(&created.id).expect("the read succeeds"),
        Some(created),
        "base_commit did not survive the round trip"
    );
}

#[test]
fn the_id_is_sixteen_lowercase_hex_characters() {
    // The shape everything the id is pasted into depends on: `agent/<id>`, `s_<id>`,
    // `.worktrees/<id>` and `<data>/sessions/<id>/` (ADR-0011).
    let dir = TempDir::new("id");
    let database = open(&dir);
    let created = database
        .sessions()
        .create(&new_session(None), NOW)
        .expect("a session is creatable");
    assert_eq!(
        created.id.len(),
        16,
        "id {} is not 16 characters",
        created.id
    );
    assert!(
        created
            .id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "id {} is not lowercase hex",
        created.id
    );
    let second = database
        .sessions()
        .create(&new_session(None), NOW)
        .expect("a second session is creatable");
    assert_ne!(created.id, second.id, "two sessions share an id");
}

#[test]
fn network_round_trips_as_a_bool() {
    let dir = TempDir::new("network");
    let database = open(&dir);
    let repository = database.sessions();
    let mut online = new_session(Some("online"));
    online.network = true;
    let created = repository
        .create(&online, NOW)
        .expect("a session is creatable");
    let read = repository
        .get(&created.id)
        .expect("the read succeeds")
        .expect("the session exists");
    assert!(
        read.network,
        "network=true did not survive the INTEGER column"
    );
    let offline = repository
        .create(&new_session(Some("offline")), NOW)
        .expect("a session is creatable");
    assert!(!offline.network);
}

// ---- read ----------------------------------------------------------------------------

#[test]
fn a_session_reads_back_by_id() {
    let dir = TempDir::new("by-id");
    let database = open(&dir);
    let repository = database.sessions();
    let created = repository
        .create(&new_session(Some("by-id")), NOW)
        .expect("a session is creatable");
    assert_eq!(
        repository.get(&created.id).expect("the read succeeds"),
        Some(created.clone())
    );
    assert_eq!(
        repository.get("no-such-id").expect("the read succeeds"),
        None
    );
}

#[test]
fn a_session_reads_back_by_client_key() {
    let dir = TempDir::new("by-key");
    let database = open(&dir);
    let repository = database.sessions();
    let created = repository
        .create(&new_session(Some("the-key")), NOW)
        .expect("a session is creatable");
    assert_eq!(
        repository
            .by_client_key("the-key")
            .expect("the read succeeds"),
        Some(created.clone())
    );
    assert_eq!(
        repository
            .by_client_key("another-key")
            .expect("the read succeeds"),
        None
    );
}

#[test]
fn many_sessions_without_a_client_key_are_not_duplicates() {
    // A create without an `Idempotency-Key` is not a duplicate, so SQLite's UNIQUE must allow
    // many NULLs (HS-008 relies on it).
    let dir = TempDir::new("no-key");
    let database = open(&dir);
    let repository = database.sessions();
    for _ in 0..3 {
        repository
            .create(&new_session(None), NOW)
            .expect("a session without a client key is creatable");
    }
    assert_eq!(repository.list(None).expect("the list reads").len(), 3);
}

#[test]
fn the_same_client_key_twice_is_refused() {
    let dir = TempDir::new("duplicate-key");
    let database = open(&dir);
    let repository = database.sessions();
    let created = repository
        .create(&new_session(Some("taken")), NOW)
        .expect("a session is creatable");
    let error = repository
        .create(&new_session(Some("taken")), NOW)
        .expect_err("a duplicate client key is refused");
    // The repository maps SQLite's `UNIQUE` violation to this variant by extended code, which is
    // what lets the handler answer the winning row instead of a 500 (HS-008, ADR-0012).
    assert!(matches!(error, RepoError::DuplicateClientKey), "{error}");
    assert_eq!(
        error.to_string(),
        "create: a session already holds this client key"
    );
    assert_eq!(
        repository
            .by_client_key("taken")
            .expect("the read succeeds"),
        Some(created),
        "the refused insert changed the row it collided with"
    );
}

#[test]
fn a_corrupt_status_is_reported_with_the_text_it_holds() {
    let dir = TempDir::new("corrupt-status");
    let created = open(&dir)
        .sessions()
        .create(&new_session(Some("corrupt")), NOW)
        .expect("a session is creatable");
    // `ended` is the value the spec removed, and the only way a row can hold it is a hand edit or
    // an older release: the repository writes `Status` only, so the corruption has to be raw SQL.
    Connection::open(dir.database())
        .expect("the database is writable")
        .execute(
            "UPDATE sessions SET status = 'ended' WHERE id = ?1",
            [&created.id],
        )
        .expect("the row is corruptible");
    let database = open(&dir);
    let repository = database.sessions();
    let error = repository
        .get(&created.id)
        .expect_err("a corrupt status is a read-time error");
    assert!(
        matches!(error, RepoError::Status(ref text) if text == "ended"),
        "{error}"
    );
    // The unfiltered list parses every row, so the corrupt one is an error there rather than
    // being dropped; a state filter that the row does not match never reads its status at all.
    assert!(matches!(
        repository.list(None),
        Err(RepoError::Status(ref text)) if text == "ended"
    ));
}

// ---- the live count ------------------------------------------------------------------

#[test]
fn live_count_counts_only_the_three_live_statuses() {
    let dir = TempDir::new("live-count");
    let database = open(&dir);
    let repository = database.sessions();
    assert_eq!(
        repository.live_count().expect("the count reads"),
        0,
        "an empty table has no live session"
    );
    for status in Status::ALL {
        let created = repository
            .create(&new_session(Some(status.as_str())), NOW)
            .expect("a session is creatable");
        repository
            .set_status(&created.id, status, None, NOW)
            .expect("the update succeeds")
            .expect("the session exists");
    }
    assert_eq!(repository.list(None).expect("the list reads").len(), 6);
    assert_eq!(
        repository.live_count().expect("the count reads"),
        3,
        "starting, idle and running are the live set (spec item 8)"
    );
}

// ---- list ----------------------------------------------------------------------------

#[test]
fn list_returns_every_session_most_recently_active_first() {
    let dir = TempDir::new("list");
    let database = open(&dir);
    let repository = database.sessions();
    let oldest = repository
        .create(&new_session(Some("oldest")), NOW)
        .expect("a session is creatable");
    let newest = repository
        .create(&new_session(Some("newest")), NOW + 20)
        .expect("a session is creatable");
    let middle = repository
        .create(&new_session(Some("middle")), NOW + 10)
        .expect("a session is creatable");
    let ids: Vec<String> = repository
        .list(None)
        .expect("the list reads")
        .into_iter()
        .map(|session| session.id)
        .collect();
    assert_eq!(ids, [newest.id, middle.id, oldest.id]);
}

#[test]
fn two_sessions_of_equal_activity_are_ordered_by_id() {
    // The order has to be total, or eviction has no defined first row.
    let dir = TempDir::new("list-ties");
    let database = open(&dir);
    let repository = database.sessions();
    let first = repository
        .create(&new_session(Some("a")), NOW)
        .expect("a session is creatable");
    let second = repository
        .create(&new_session(Some("b")), NOW)
        .expect("a session is creatable");
    let mut expected = [first.id.clone(), second.id.clone()];
    expected.sort();
    let ids: Vec<String> = repository
        .list(None)
        .expect("the list reads")
        .into_iter()
        .map(|session| session.id)
        .collect();
    assert_eq!(ids, expected);
}

#[test]
fn list_with_a_status_returns_only_that_status() {
    let dir = TempDir::new("list-filter");
    let database = open(&dir);
    let repository = database.sessions();
    let running = repository
        .create(&new_session(Some("running")), NOW)
        .expect("a session is creatable");
    let idle = repository
        .create(&new_session(Some("idle")), NOW + 1)
        .expect("a session is creatable");
    repository
        .set_status(&running.id, Status::Running, None, NOW)
        .expect("the update succeeds");
    repository
        .set_status(&idle.id, Status::Idle, None, NOW)
        .expect("the update succeeds");
    let ids = |status| -> Vec<String> {
        repository
            .list(Some(status))
            .expect("the filtered list reads")
            .into_iter()
            .map(|session| session.id)
            .collect()
    };
    assert_eq!(ids(Status::Running), std::slice::from_ref(&running.id));
    assert_eq!(ids(Status::Idle), std::slice::from_ref(&idle.id));
    assert_eq!(ids(Status::Stopped), Vec::<String>::new());
    assert_eq!(repository.list(None).expect("the list reads").len(), 2);
}

// ---- update --------------------------------------------------------------------------

#[test]
fn set_status_records_the_status_and_its_reason() {
    let dir = TempDir::new("set-status");
    let database = open(&dir);
    let repository = database.sessions();
    let created = repository
        .create(&new_session(Some("status")), NOW)
        .expect("a session is creatable");
    let updated = repository
        .set_status(
            &created.id,
            Status::Error,
            Some("provisioning_failed"),
            NOW + 5,
        )
        .expect("the update succeeds")
        .expect("the session exists");
    assert_eq!(updated.status, Status::Error);
    assert_eq!(
        updated.status_reason.as_deref(),
        Some("provisioning_failed")
    );
    assert_eq!(updated.stopped_at, None, "error is not stopped");
    assert_eq!(
        repository.get(&created.id).expect("the read succeeds"),
        Some(updated),
        "the update did not reach the file"
    );
}

#[test]
fn set_status_sets_stopped_at_only_while_the_session_is_stopped() {
    let dir = TempDir::new("stopped-at");
    let database = open(&dir);
    let repository = database.sessions();
    let created = repository
        .create(&new_session(Some("stopped-at")), NOW)
        .expect("a session is creatable");
    let stopped = repository
        .set_status(&created.id, Status::Stopped, Some("idle_stop"), NOW + 60)
        .expect("the update succeeds")
        .expect("the session exists");
    assert_eq!(stopped.stopped_at, Some(NOW + 60), "stopped_at was not set");
    assert_eq!(stopped.status_reason.as_deref(), Some("idle_stop"));

    // Resume: `starting` clears it (item 15's "cleared on resume").
    let resumed = repository
        .set_status(&created.id, Status::Starting, None, NOW + 120)
        .expect("the update succeeds")
        .expect("the session exists");
    assert_eq!(resumed.stopped_at, None, "stopped_at was not cleared");
    assert_eq!(resumed.status_reason, None);
    assert_eq!(resumed.status, Status::Starting);
}

#[test]
fn set_status_leaves_last_active_at_alone() {
    // Activity is not status: HS-020 and HS-021 own when `last_active_at` moves.
    let dir = TempDir::new("last-active");
    let database = open(&dir);
    let repository = database.sessions();
    let created = repository
        .create(&new_session(Some("activity")), NOW)
        .expect("a session is creatable");
    let updated = repository
        .set_status(&created.id, Status::Running, None, NOW + 600)
        .expect("the update succeeds")
        .expect("the session exists");
    assert_eq!(updated.last_active_at, NOW);
    assert_eq!(updated.created_at, NOW);
}

#[test]
fn set_status_on_an_unknown_id_returns_none() {
    let dir = TempDir::new("unknown-id");
    let database = open(&dir);
    assert_eq!(
        database
            .sessions()
            .set_status("no-such-id", Status::Stopped, None, NOW)
            .expect("the update succeeds"),
        None
    );
}

#[test]
fn touch_moves_last_active_at_and_nothing_else() {
    // HS-015: the turn runner touches at the prompt and again at the settle, and neither is a
    // state change, so the status write deliberately leaves the column alone and this is the only
    // statement that moves it.
    let dir = TempDir::new("touch");
    let database = open(&dir);
    let repository = database.sessions();
    let created = repository
        .create(&new_session(Some("touch")), NOW)
        .expect("a session is creatable");
    repository
        .set_status(&created.id, Status::Running, None, NOW + 10)
        .expect("the update succeeds")
        .expect("the session exists");
    repository
        .touch(&created.id, NOW + 20)
        .expect("the touch succeeds");
    let touched = repository
        .get(&created.id)
        .expect("the read succeeds")
        .expect("the session exists");
    assert_eq!(touched.last_active_at, NOW + 20);
    assert_eq!(touched.created_at, NOW, "a touch is not a create");
    assert_eq!(touched.status, Status::Running, "a touch is not a state");
    assert_eq!(touched.status_reason, None);
}

#[test]
fn touch_succeeds_for_an_unknown_id() {
    // A turn's task can lose its session to a delete or a recovery, and there is nothing to record
    // then: an error would turn an ordinary race into a logged failure.
    let dir = TempDir::new("touch-unknown");
    let database = open(&dir);
    database
        .sessions()
        .touch("no-such-id", NOW)
        .expect("touching a row that is gone is not a failure");
}

#[test]
fn set_worktree_records_the_path_and_the_base_and_nothing_else() {
    // HS-025: the provisioner writes what the worktree step made. It is not a state change — the session
    // stays `starting` while the next step runs — and it must not disturb the two timestamps, because
    // `last_active_at` is activity's and the cap's eviction order reads it.
    let dir = TempDir::new("set-worktree");
    let database = open(&dir);
    let repository = database.sessions();
    let created = repository
        .create(&new_session(Some("set-worktree")), NOW)
        .expect("a session is creatable");
    let base = "5f2c1d0e9b8a7c6d5e4f3a2b1c0d9e8f7a6b5c4d";
    repository
        .touch(&created.id, NOW + 20)
        .expect("the touch succeeds");
    repository
        .set_worktree(&created.id, "/srv/projects/.worktrees/an-id", base)
        .expect("the record is writable");
    let recorded = repository
        .get(&created.id)
        .expect("the read succeeds")
        .expect("the session exists");
    assert_eq!(recorded.worktree, "/srv/projects/.worktrees/an-id");
    assert_eq!(recorded.base_commit.as_deref(), Some(base));
    assert_eq!(recorded.status, Status::Starting, "a record is not a state");
    assert_eq!(recorded.status_reason, None);
    assert_eq!(
        recorded.last_active_at,
        NOW + 20,
        "a record is not activity"
    );
    assert_eq!(recorded.created_at, NOW);
}

#[test]
fn set_worktree_succeeds_for_an_unknown_id() {
    // The worktree step's record is written by a background task, which can lose its session to a delete
    // while git is running. The worktree went with the row, so there is nothing to record.
    let dir = TempDir::new("set-worktree-unknown");
    let database = open(&dir);
    database
        .sessions()
        .set_worktree("no-such-id", "/srv/projects/.worktrees/an-id", "abc123")
        .expect("recording a row that is gone is not a failure");
}

// ---- the demo ------------------------------------------------------------------------

#[test]
fn a_session_survives_a_restart_and_reads_back_identically() {
    let dir = TempDir::new("restart");
    let created = {
        let database = open(&dir);
        let created = database
            .sessions()
            .create(&new_session(Some("restart")), NOW)
            .expect("a session is creatable");
        database
            .sessions()
            .set_status(&created.id, Status::Idle, None, NOW + 1)
            .expect("the update succeeds");
        created
    };
    // A second `Database` on the same file is the restart: nothing of the first one is in memory.
    let database = open(&dir);
    let read = database
        .sessions()
        .get(&created.id)
        .expect("the read succeeds")
        .expect("the session survived the restart");
    // The whole struct in one assertion: only the status the update set differs from the row the
    // create returned.
    let mut expected = created;
    expected.status = Status::Idle;
    assert_eq!(read, expected);
}
