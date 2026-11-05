//! The previous release's schema, compared with this one (HS-075, spec item 10, ADR-0047).
//!
//! Item 10's rule is "a release may add but never drop or rename anything the previous release uses". The
//! destructive side is already refused at the point it is written (`schema::contract_violations`, ADR-0009); what
//! this file adds is the other half: a **fact** about the release that is leaving, checked against the release that
//! is arriving.
//!
//! `docs/schema/previous.sql` is a real SQL dump, and SQLite compares it: the snapshot goes into one temporary
//! database and the binary's own migrations into another, and every table and column the snapshot names must exist
//! in the second. That is a subset test on purpose — an added column is the whole point of "expand", so a test that
//! demanded equality would forbid the rule it is checking.
//!
//! Two guards on the guard, because a subset test over a broken snapshot passes vacuously: the snapshot must name
//! a non-trivial number of columns, and it must name every table this build has. The second is what catches a
//! snapshot refreshed from the wrong database.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;

use holosmith::db::{Database, OpenOptions};
use holosmith::schema::{self, Verdict};

mod support;
use support::TempDir;

/// The previous release's schema, as `scripts/schema-snapshot.sh` wrote it. `include_str!` rather than a read at
/// run time, so a test binary built against a checkout without the file fails to compile instead of skipping.
const PREVIOUS: &str = include_str!("../../../docs/schema/previous.sql");

/// The clock these tests write, so nothing is raced.
const NOW: i64 = 1_760_000_000;

/// Every table and its columns, read from SQLite rather than parsed out of the SQL: this repository has one SQL
/// scanner (`src/sql.rs`) and a second grammar over `CREATE TABLE` would be a second thing to be wrong.
fn schema_of(connection: &Connection) -> BTreeMap<String, BTreeSet<String>> {
    let mut tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .expect("sqlite_master is readable")
        .query_map([], |row| row.get(0))
        .expect("the table list is a query")
        .collect::<Result<_, _>>()
        .expect("every table name is text");
    // `sqlite_master`'s order is not a promise, and neither is a map's: the comparison sorts.
    tables.sort();
    let mut schema = BTreeMap::new();
    for table in tables {
        let mut columns: Vec<String> = connection
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .expect("pragma_table_info is a table-valued function")
            .query_map([&table], |row| row.get(0))
            .expect("the column list is a query")
            .collect::<Result<_, _>>()
            .expect("every column name is text");
        columns.sort();
        schema.insert(table, columns.into_iter().collect());
    }
    schema
}

/// The snapshot, executed into a database of its own.
fn previous() -> BTreeMap<String, BTreeSet<String>> {
    let connection = Connection::open_in_memory().expect("an in-memory database opens");
    connection
        .execute_batch(PREVIOUS)
        .expect("docs/schema/previous.sql is executable SQL");
    schema_of(&connection)
}

/// This build's schema, migrated the way `serve` migrates it.
///
/// Its own `TempDir` label per test: `support::TempDir` is keyed by label **and** pid, so two tests in one binary
/// that share a label remove each other's directory mid-run (a documented hazard, and a "disk I/O error" from
/// SQLite is what it looks like).
fn current(label: &str) -> (TempDir, BTreeMap<String, BTreeSet<String>>) {
    let dir = TempDir::new(label);
    let database = Database::open(&dir.database(), OpenOptions::default(), NOW)
        .expect("the embedded migrations apply");
    assert_eq!(database.schema_version(), schema::SCHEMA_VERSION);
    let connection = Connection::open(dir.database()).expect("the database reopens");
    let schema = schema_of(&connection);
    drop(connection);
    drop(database);
    (dir, schema)
}

/// The rule, as a function: everything the previous schema names that the current one does not.
///
/// A rename is named twice on purpose — `sessions.old_name` is gone and `sessions.new_name` is new — because both
/// are what a reader of the old schema loses, and the pair is what tells a rename from a drop in the message.
fn missing(
    previous: &BTreeMap<String, BTreeSet<String>>,
    current: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<String> {
    let mut absent = Vec::new();
    for (table, columns) in previous {
        match current.get(table) {
            None => absent.push(format!("table {table}")),
            Some(now) => {
                for column in columns {
                    if !now.contains(column) {
                        absent.push(format!("{table}.{column}"));
                    }
                }
            }
        }
    }
    absent.sort();
    absent
}

#[test]
fn the_previous_releases_schema_is_still_a_subset_of_this_one() {
    let (dir, now) = current("schema-previous-subset");
    let then = previous();

    // Non-vacuity, twice. A snapshot that names nothing passes every assertion below, and one refreshed from a
    // database of some other build still passes a subset test that only looks at what the two happen to share —
    // so the file is required to name this schema's tables, not merely to be a subset of it.
    let named: usize = then.values().map(BTreeSet::len).sum();
    assert!(
        then.len() >= 3 && named >= 20,
        "the snapshot names {} tables and {named} columns, which is not this schema ({})",
        then.len(),
        dir.path().display()
    );
    assert_eq!(
        then.keys().collect::<Vec<_>>(),
        now.keys().collect::<Vec<_>>(),
        "the snapshot and this build disagree about which tables exist — one was dropped, or the snapshot was \
         refreshed from another build ({})",
        dir.path().display()
    );

    assert!(
        missing(&then, &now).is_empty(),
        "this release dropped or renamed {:?}, which the previous release still reads. Either expand only (keep \
         the old shape, and contract in a later release as `NNNN_contract_*.sql` — ADR-0047), or refresh \
         docs/schema/previous.sql in the same commit and say in the ADR why rolling back past this release is \
         acceptable",
        missing(&then, &now)
    );
}

#[test]
fn a_dropped_column_a_dropped_table_and_a_rename_are_all_named() {
    // The rule labelled with each shape it must catch, so a comparison that stopped comparing cannot pass the
    // snapshot test by being empty (the HS-061 lesson: a test's placement decides what it can prove).
    let then: BTreeMap<String, BTreeSet<String>> = [
        (
            "sessions".to_string(),
            ["id", "usage_in", "usage_out"].map(str::to_string).into(),
        ),
        ("events".to_string(), ["seq"].map(str::to_string).into()),
        ("gone".to_string(), ["body"].map(str::to_string).into()),
    ]
    .into();
    let now: BTreeMap<String, BTreeSet<String>> = [
        // `usage_in` dropped and `usage_out` renamed to `out_tokens`: both must be reported, and `id` must not.
        (
            "sessions".to_string(),
            ["id", "out_tokens"].map(str::to_string).into(),
        ),
        (
            "events".to_string(),
            ["seq", "payload"].map(str::to_string).into(),
        ),
    ]
    .into();
    assert_eq!(
        missing(&then, &now),
        // One line for a dropped table rather than one per column: the table is the finding, and its columns are
        // gone with it.
        vec![
            "sessions.usage_in".to_string(),
            "sessions.usage_out".to_string(),
            "table gone".to_string(),
        ]
    );
    // And an added column is invisible to it, which is the half of the rule that must not fail.
    let wider: BTreeMap<String, BTreeSet<String>> = [
        (
            "sessions".to_string(),
            ["id", "usage_in", "usage_out", "added_later"]
                .map(str::to_string)
                .into(),
        ),
        ("events".to_string(), ["seq"].map(str::to_string).into()),
        ("gone".to_string(), ["body"].map(str::to_string).into()),
        (
            "added_later_table".to_string(),
            ["x"].map(str::to_string).into(),
        ),
    ]
    .into();
    assert!(
        missing(&then, &wider).is_empty(),
        "{:?}",
        missing(&then, &wider)
    );
}

#[test]
fn a_newer_database_is_not_this_binarys_schema_and_says_so() {
    // The verdict `doctor` acts on (HS-045) and the sentence `apply_pending` warns with, without a database: the
    // acceptance's third bullet, as the part of it that is this story's to build.
    assert_eq!(schema::verdict(3, 4), Verdict::Older);
    assert_eq!(schema::verdict(4, 4), Verdict::Same);
    assert_eq!(schema::verdict(99, 4), Verdict::Newer);

    let older = Verdict::Older.sentence(3, 4);
    assert!(older.contains('3') && older.contains('4'), "{older}");
    let newer = Verdict::Newer.sentence(99, 4);
    for needle in ["99", "4", "doctor", "restore"] {
        assert!(newer.contains(needle), "{needle} is not in {newer}");
    }
    assert!(
        !Verdict::Same.sentence(4, 4).contains("doctor"),
        "the current schema is not a finding"
    );
}
