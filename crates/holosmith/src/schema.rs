//! The numbered SQL migrations this binary embeds, and the constant `version` reports.
//!
//! Migrations are forward-only files in `crates/holosmith/migrations`, four-digit numbered,
//! embedded at compile time; `SCHEMA_VERSION` is a literal because `version --json` must answer
//! without opening a database (ADR-0009). Two guards live here — the names have to be a
//! contiguous run from 1, and the highest number has to equal the constant — and a third,
//! `Database::open` refusing to start, is what makes the constant true of the artifact rather
//! than only of `cargo test`.

use std::fmt;

use include_dir::{Dir, include_dir};

use crate::sql::code_only;

/// The newest database migration this binary embeds.
pub const SCHEMA_VERSION: u32 = 4;

static MIGRATIONS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/migrations");

/// One embedded migration: its number, its slug and the SQL it runs.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub number: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

impl Migration {
    /// The file this migration came from, as an operator would name it.
    pub fn file(&self) -> String {
        format!("{:04}_{}.sql", self.number, self.name)
    }
}

/// Why the embedded migrations cannot be used.
#[derive(Debug, PartialEq, Eq)]
pub enum EmbedError {
    /// A file in the migrations directory does not match `NNNN_slug.sql`.
    Name { file: String },
    /// A file in the migrations directory is not UTF-8, so it cannot be a SQL migration.
    Contents { file: String },
    /// The numbers are not a contiguous run from 1, which also covers duplicates.
    Order { previous: u32, next: u32 },
    /// The highest embedded number and `SCHEMA_VERSION` disagree: a stale embed, or a file
    /// added without the constant.
    Disagrees { embedded: u32, constant: u32 },
}

impl fmt::Display for EmbedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name { file } => write!(
                f,
                "migration {file} is not named NNNN_slug.sql (four digits, a-z, 0-9 and _ only)"
            ),
            Self::Contents { file } => write!(f, "migration {file} is not UTF-8 text"),
            Self::Order { previous, next } => write!(
                f,
                "the embedded migrations are not numbered 1..=n: {next:04} follows {previous:04}"
            ),
            Self::Disagrees { embedded, constant } => write!(
                f,
                "the embedded migrations stop at {embedded}, but this binary reports schema \
                 {constant}: add the migration or fix SCHEMA_VERSION in src/schema.rs"
            ),
        }
    }
}

impl std::error::Error for EmbedError {}

/// Every embedded migration, ordered by number.
pub fn embedded() -> Result<Vec<Migration>, EmbedError> {
    let mut migrations = Vec::new();
    for file in MIGRATIONS.files() {
        let path = file.path();
        let name = path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| EmbedError::Name {
                file: path.display().to_string(),
            })?;
        let (number, slug) = parse_name(name).ok_or_else(|| EmbedError::Name {
            file: name.to_string(),
        })?;
        let sql = file.contents_utf8().ok_or_else(|| EmbedError::Contents {
            file: name.to_string(),
        })?;
        migrations.push(Migration {
            number,
            name: slug,
            sql,
        });
    }
    migrations.sort_by_key(|migration| migration.number);
    Ok(migrations)
}

/// `^([0-9]{4})_([a-z0-9_]+)\.sql$`, without a regex in the dependency tree.
fn parse_name(name: &'static str) -> Option<(u32, &'static str)> {
    let stem = name.strip_suffix(".sql")?;
    let (digits, slug) = stem.split_once('_')?;
    if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let slug_is_a_name = !slug.is_empty()
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if !slug_is_a_name {
        return None;
    }
    let number = digits.parse().ok()?;
    Some((number, slug))
}

/// The agreement guard: the migrations have to be 1, 2, … n and n has to be `SCHEMA_VERSION`.
///
/// `Database::open` calls this on the embedded set, and the tests call it with a literal slice
/// so each refusal is proved to fire.
pub fn check_embed(migrations: &[Migration]) -> Result<u32, EmbedError> {
    let mut previous = 0;
    for migration in migrations {
        if migration.number != previous + 1 {
            return Err(EmbedError::Order {
                previous,
                next: migration.number,
            });
        }
        previous = migration.number;
    }
    if previous != SCHEMA_VERSION {
        return Err(EmbedError::Disagrees {
            embedded: previous,
            constant: SCHEMA_VERSION,
        });
    }
    Ok(previous)
}

/// What a database's recorded schema version means to this binary (HS-075, spec item 10).
///
/// One function rather than a comparison at each caller, because the callers must not disagree:
/// [`crate::db::apply_pending`] decides whether to migrate, the startup banner prints the number this verdict is
/// about (the **recorded** one), and `doctor` (HS-045) will decide whether to refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The database is already the schema this binary embeds.
    Same,
    /// The database is behind: this binary migrates it on the next open.
    Older,
    /// A **newer** release wrote this database. Item 10's rollback is the reason it is not fatal to `serve`:
    /// switching `current` back to an older binary must not need a restore, and expand/contract is what makes
    /// continuing safe. `doctor` is where it refuses, because that is the guard that runs before a start —
    /// [`crate::doctor`] reads it through [`crate::db::recorded_without_migrating`], which does not migrate.
    Newer,
}

/// What `recorded` means to a binary that understands `binary`.
pub const fn verdict(recorded: u32, binary: u32) -> Verdict {
    if recorded == binary {
        Verdict::Same
    } else if recorded < binary {
        Verdict::Older
    } else {
        Verdict::Newer
    }
}

impl Verdict {
    /// The sentence an operator reads, which is the same one in the banner's words, in the startup `warn`, and in
    /// `doctor`'s refusal — one spelling of one fact (HS-020's lesson, one level up).
    pub fn sentence(self, recorded: u32, binary: u32) -> String {
        match self {
            Self::Same => format!("the database is at this release's schema ({binary})"),
            Self::Older => format!(
                "the database is at schema {recorded} and this release migrates it to {binary}; the migrations it \
                 runs are forward-only and additive, so the previous release could still read the result"
            ),
            Self::Newer => format!(
                "the database is at schema {recorded}, which a newer release wrote, and this binary understands \
                 {binary}: `doctor` refuses to start on it, and `serve` continues only because rolling `current` \
                 back must not need a database restore — that is what expand/contract buys (spec item 10, \
                 ADR-0047)"
            ),
        }
    }
}

/// Migrations that drop or rename something without being named as contract migrations.
///
/// ADR-0009's expand/contract rule: a destructive statement ships only in
/// `NNNN_contract.sql` or `NNNN_contract_*.sql`. This is the half that one file can answer — is this statement
/// destructive at all, and was that deliberate? — and it is enforced: `cargo test` runs it over the embedded set.
///
/// The other half is the release-over-release question, which no single file can see: `tests/schema_previous.rs`
/// compares `docs/schema/previous.sql` (the schema the previous release shipped) with this build's, as a subset,
/// and ADR-0047 records both halves and how the snapshot is refreshed. A `_contract_` migration makes this
/// function quiet and is then caught by that test until the snapshot is refreshed in the same commit.
pub fn contract_violations(migrations: &[Migration]) -> Vec<String> {
    migrations
        .iter()
        .filter(|migration| !is_contract_migration(migration.name))
        .filter_map(|migration| {
            destructive_statement(&code_only(migration.sql)).map(|statement| {
                format!(
                    "{} runs {statement}: name the file NNNN_contract_*.sql",
                    migration.file()
                )
            })
        })
        .collect()
}

/// `NNNN_contract.sql` or `NNNN_contract_*.sql`; `decontract_legacy` is not a contract
/// migration, which is why this is a prefix test and not `contains`.
fn is_contract_migration(name: &str) -> bool {
    name == "contract" || name.starts_with("contract_")
}

/// The destructive statement `code` holds, if any, matched on **words** so that a table called
/// `renamed_things` is not a rename and `CREATE INDEX rename_idx` is not either.
fn destructive_statement(code: &str) -> Option<&'static str> {
    let words: Vec<String> = code
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    for pair in words.windows(2) {
        match (pair[0].as_str(), pair[1].as_str()) {
            ("DROP", "TABLE") => return Some("DROP TABLE"),
            ("DROP", "COLUMN") => return Some("DROP COLUMN"),
            _ => {}
        }
    }
    if words.iter().any(|word| word == "RENAME") {
        return Some("RENAME");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literal(number: u32) -> Migration {
        Migration {
            number,
            name: "x",
            sql: "",
        }
    }

    #[test]
    fn every_embedded_file_is_a_numbered_sql_name() {
        assert_eq!(parse_name("0001_initial.sql"), Some((1, "initial")));
        assert_eq!(
            parse_name("0012_events_ordered.sql"),
            Some((12, "events_ordered"))
        );
        for refused in [
            "1_initial.sql",
            "00001_initial.sql",
            "0001_Initial.sql",
            "0001-initial.sql",
            "0001_initial.SQL",
            "0001_initial",
            "0001_.sql",
            "_initial.sql",
            "initial.sql",
        ] {
            assert_eq!(parse_name(refused), None, "{refused} must be refused");
        }
    }

    #[test]
    fn the_embedded_set_runs_from_one_without_a_gap() {
        let migrations = embedded().expect("the embedded migrations are well formed");
        assert!(!migrations.is_empty(), "no migration is embedded");
        assert_eq!(check_embed(&migrations), Ok(SCHEMA_VERSION));
        let numbers: Vec<u32> = migrations
            .iter()
            .map(|migration| migration.number)
            .collect();
        assert_eq!(numbers, (1..=SCHEMA_VERSION).collect::<Vec<u32>>());
    }

    #[test]
    fn the_highest_embedded_number_is_the_reported_schema_version() {
        let migrations = embedded().expect("the embedded migrations are well formed");
        let highest = migrations
            .last()
            .map(|migration| migration.number)
            .unwrap_or_default();
        assert_eq!(highest, SCHEMA_VERSION);
    }

    #[test]
    fn a_gap_in_the_numbers_is_refused() {
        let migrations = [literal(1), literal(3)];
        assert!(matches!(
            check_embed(&migrations),
            Err(EmbedError::Order {
                previous: 1,
                next: 3
            })
        ));
    }

    #[test]
    fn a_duplicate_number_is_refused() {
        let migrations = [literal(1), literal(1)];
        assert!(matches!(
            check_embed(&migrations),
            Err(EmbedError::Order {
                previous: 1,
                next: 1
            })
        ));
    }

    #[test]
    fn a_set_that_stops_short_of_the_constant_is_refused() {
        // One migration past the constant, so this keeps working when the schema grows.
        let migrations: Vec<Migration> = (1..=SCHEMA_VERSION + 1).map(literal).collect();
        assert!(matches!(
            check_embed(&migrations),
            Err(EmbedError::Disagrees {
                embedded,
                constant: SCHEMA_VERSION
            }) if embedded == SCHEMA_VERSION + 1
        ));
    }

    #[test]
    fn a_destructive_statement_outside_a_contract_migration_is_refused() {
        // ADR-0009/ADR-0047: this half of expand/contract is enforced by the name, and each case below proves the
        // rule fires rather than passing over today's one file. The release-over-release half is
        // `tests/schema_previous.rs`.
        let migration = |name: &'static str, sql: &'static str| Migration {
            number: 2,
            name,
            sql,
        };
        let refused = contract_violations(&[migration("drop_events", "DROP TABLE events;")]);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(
            refused[0].starts_with("0002_drop_events.sql"),
            "{refused:?}"
        );
        assert!(refused[0].contains("DROP TABLE"), "{refused:?}");
        assert_eq!(
            contract_violations(&[migration(
                "cleanup",
                "ALTER TABLE events DROP COLUMN payload;"
            )])
            .len(),
            1
        );
        assert_eq!(
            contract_violations(&[migration("cleanup", "ALTER TABLE events RENAME TO logs;")])
                .len(),
            1
        );

        for contract in ["contract", "contract_drop_events"] {
            assert!(
                contract_violations(&[migration(contract, "DROP TABLE events;")]).is_empty(),
                "{contract} is a contract migration"
            );
        }
        // `decontract_legacy` is not one: `contains("contract")` would have accepted it.
        assert_eq!(
            contract_violations(&[migration("decontract_legacy", "DROP TABLE events;")]).len(),
            1
        );
        // Prose is not a statement, and neither is a name that merely starts with a keyword.
        assert!(
            contract_violations(&[migration(
                "add_renamed_at",
                "-- renamed_at replaces the old column\n\
             ALTER TABLE events ADD COLUMN renamed_at INTEGER;\n\
             CREATE INDEX rename_idx ON events (renamed_at);\n\
             INSERT INTO notes VALUES ('DROP TABLE events');"
            )])
            .is_empty()
        );
        // One file of a set is enough: a contract migration beside an additive one is quiet,
        // and a second, unmarked destructive file is still reported.
        assert!(
            contract_violations(&[
                migration("contract_drop_events", "DROP TABLE events;"),
                migration("add_note", "CREATE TABLE notes (body TEXT);"),
            ])
            .is_empty()
        );
        let mixed = contract_violations(&[
            migration("contract_drop_events", "DROP TABLE events;"),
            migration("drop_logs", "DROP TABLE logs;"),
        ]);
        assert_eq!(mixed.len(), 1, "{mixed:?}");
        assert!(mixed[0].starts_with("0002_drop_logs.sql"), "{mixed:?}");
    }

    #[test]
    fn the_embedded_migrations_carry_no_contract_violation() {
        let migrations = embedded().expect("the embedded migrations are well formed");
        let violations = contract_violations(&migrations);
        assert!(violations.is_empty(), "{violations:?}");
    }
}
