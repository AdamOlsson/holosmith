# Review — PER-39 (HS-007)

**Fixed point:** `108a6ee` (the last commit before this work), reviewed against the **uncommitted**
tree. In-scope files read in full: `crates/holosmith/migrations/0002_sessions.sql`,
`src/sessions/{mod,repository}.rs`, `src/db.rs`, `src/schema.rs`, `src/lib.rs`,
`tests/session_repository.rs`, the `tests/db_migrations.rs` edits, `docs/adr/0011-*`, and the
`GLOSSARY.md` diff, plus `src/sql.rs`, `HolosmithSpec.md` "Data model and API changes" (~270), the
`stopped_at` rule (line 260), LRU (line 156), the HS-007 slice and the approved plan.

**Verification basis.** This role has no shell, so it did not execute the gates; it read the diff
artifact and the tree. The supervisor ran, after sourcing the toolchain env: `cargo fmt --check`;
`cargo clippy --all-targets -- -D warnings`; `cargo test` (138 lib, 20 config_cli, 12 db_migrations,
39 guards, 11 http_contract, 2 pin_single_source, 12 serve_cli, 19 session_repository, 4
version_cli); `scripts/verify.sh` end to end (exit 0, 11 cases + the CORS sweep, 31 assertions); and
the restart demo — before and after the fixes below.

**Verdict:** Standards OK with notes, Spec OK with notes. No blocker. Three should-fix items and four
nits were fixed; one nit was dismissed. The review's sharpest find was structural: this story broke
HS-006's own "every later story adds a `case_*`" rule, one story after writing it.


Method note: `watchdog_diff` reported *no* delta against launch HEAD `108a6ee`, so I reviewed the tree as it exists on disk plus `/tmp/holosmith/reviews/PER-39-diff.txt`. I have no shell, so nothing below is execution-verified; every claim is from source. Gates still worth re-running by the supervisor: `cargo test --test session_repository` and `scripts/verify.sh` (see S1).

---

## Standards

### What holds

- **Layout and doc style match the repo.** `sessions/mod.rs:1-8` and `sessions/repository.rs:1-6` carry module headers; every `pub` item has a doc comment; tests live in `#[cfg(test)] mod tests` at the bottom (as in `schema.rs`/`sql.rs`) plus one integration file. Naming follows ADR-0009 (`migrations/0002_sessions.sql`, `SCHEMA_VERSION = 2` in `schema.rs:17`, `check_embed` enforces the pair).
- **The privacy decisions hold in code, not just prose.** `Database::connection` is private (`db.rs:47-52`), there is no accessor anywhere in `src/` (grep for `fn connection|pub fn conn` finds only `db.rs:218`/`db.rs:329`, both private helpers), `SessionRepository::new` is `pub(crate)` (`repository.rs:44`), and `Database::sessions()` is the sole seam (`db.rs:105-107`).
- **`now` convention matches the machine it extends.** `create`/`set_status` take `now: i64` exactly as `Database::open` does (`db.rs:66`), and no repository code reads a clock.
- **No new dependency.** The diff touches no `Cargo.toml`/`Cargo.lock`; the id comes from SQLite (`repository.rs:21`), matching ADR-0011's rejection of `uuid`.
- **The migration does not trip the `_contract_` guard.** `schema::contract_violations` reads `code_only(sql)` (`schema.rs:148-166`), so the `-- ''`/`-- 16 lowercase hex` prose in `0002_sessions.sql` is invisible; there is no `DROP`/`RENAME` word in code.
- **`tests/db_migrations.rs` edits are the minimum the version bump requires** and stay honest: the embed test now derives its literal set from `SCHEMA_VERSION` (`tests/db_migrations.rs:105-110`), and the three applied-set assertions name both files rather than loosening to a count.

### Findings

**S1 — should-fix: no `case_*` added to `scripts/verify.sh`, against a documented per-story rule.**
`docs/build.md` (One-command verification): "**Every later story adds at least one `case_*` function and one line in `serve_cases`.** That is what makes this a growing gate rather than a snapshot: a story that adds a route or a behaviour has one obvious place to prove the shipped binary does it." `scripts/verify.sh:159-161` repeats it in the source comment. The change set contains no `scripts/` edit, and the plan (`/tmp/holosmith/plans/PER-39.md`, steps 1-8) never asks for one.
Nothing else covers the shipped-artifact claim: `case_schema_version` (`scripts/verify.sh:206-214`) derives the number from `version --json`, so the schema *bump* is auto-covered, but no case proves the booted artifact creates `sessions`.
*Smallest fix:* one `case_sessions_table` that reads the bytes of `$WORK/data/app.db` (`grep -aq 'sessions_status'`, no `sqlite3` needed) and one line in `serve_cases`; cite HS-007's acceptance "migration adds `sessions`".

**S2 — should-fix: the SQL-location guard's *wiring* is unproven, so the test named for a planted statement does not plant one.**
`repository.rs:224-247` walks `src/**` and collects `format!("{file}:{}: {statement}", index + 1)`; `repository.rs:250-277` is `the_guard_fires_on_a_planted_statement`, and it only feeds strings to the matcher:

```rust
for line in ["SELECT * FROM sessions WHERE id = ?1", …] {
    assert!(sessions_statement(line).is_some(), "{line} must be a sessions statement");
}
```

Empty the loop body at `repository.rs:236-240` (`if let Some(statement) = sessions_statement(line)` → nothing pushed) and **every test still passes**: `no_sessions_sql_outside_the_repository` asserts only `offenders.is_empty()`, and the predicate test never touches the walk. That is exactly the failure mode this repo already fixed once — `docs/review/PER-36.md:88-94` (S2: "the `_contract_` guard could not fire") and ADR-0009's "the test asserts on literal slices, so the rule is proved to fire instead of passing over today's single migration".
*Smallest fix:* extract `fn offenders_under(root: &Path) -> Vec<String>` from `repository.rs:225-241`, call it with `&root` in the guard test, and add one test over a `TempDir` containing `planted.rs` with `let sql = "SELECT id FROM sessions";` asserting the report equals `["planted.rs:1: from sessions"]`.

#### S3 — nit: four spellings sit outside the guard's patterns, because it matches literal substrings per line.
`STATEMENTS` (`repository.rs:183-188`) is `delete from sessions|from sessions|into sessions|update sessions`, and matching is per line (`repository.rs:236`, `text.lines()`). So all of these pass the guard if planted in another `src/` file: `SELECT … FROM main.sessions`, `FROM "sessions"`, `UPDATE OR REPLACE sessions`, and any statement split across a Rust line continuation (`"SELECT id FROM \\\n sessions"`, which is one runtime statement and two source lines). ADR-0011's claim ("fails if `FROM sessions`, `INTO sessions`, `UPDATE sessions` or `DELETE FROM sessions` appears in any other file") matches what is implemented, so this is hardening rather than a contract break.
*Smallest fix:* match against the whole file text with `\\\n` + runs of whitespace collapsed to one space before applying `names_word`, and add `"main.sessions"`/`"\"sessions\""` normalisation to the same helper.

#### S4 — nit: `TempDir` is duplicated verbatim across test binaries.
`tests/session_repository.rs:20-43` vs `tests/db_migrations.rs:21-48`: same `env::temp_dir().join(format!("holosmith-…-{pid}"))`, same `remove_dir_all` on create and on `Drop`. `tests/support/mod.rs:1-8` is the repo's documented home for helpers shared across test binaries (and already carries the `#![allow(dead_code)]` rationale for doing so).
*Smallest fix:* none required now; if a third copy appears, lift `TempDir` into `tests/support/mod.rs` with a label prefix parameter.

#### S5 — nit: `RepoError::Sqlite` drops the subject, unlike this repo's other SQLite error.
`db.rs:194-198` documents the convention "it names the file it is about"; the new `RepoError` (`sessions/mod.rs:110-128`) prints a bare rusqlite string. A `UNIQUE constraint failed: sessions.client_key` happens to name the column, but the failures HS-008 will surface from `list`/`set_status` (`no such table`, `database is locked`) reach the operator with no operation attached.
*Smallest fix:* `Sqlite { statement: &'static str, source: rusqlite::Error }` with the `From` impl taking the statement name at each of the six call sites.

## Standards verdict: **OK with notes.** Two should-fix items (S1, S2), both narrow and mechanical; three nits. No blocker, no fabricated standard, and the two strongest findings are backed by rules already written down in `docs/build.md` and applied in `docs/review/PER-36.md`.

---

## Spec

### What holds

- **The migration matches the spec fragment column for column.** `migrations/0002_sessions.sql:7-22` vs `HolosmithSpec.md:278-285`: `worktree TEXT NOT NULL`, `status TEXT NOT NULL`, `status_reason TEXT`, `client_key TEXT UNIQUE`, `network INTEGER NOT NULL DEFAULT 0`, `stopped_at INTEGER`, `usage_in`/`usage_out INTEGER NOT NULL DEFAULT 0` — all identical; plus the seven the story's second box adds (`id`, `project`, `model`, `thinking`, `base_commit`, `created_at`, `last_active_at`). No `CHECK` on `status` (item 8's machine names are the enum, and `ALTER TABLE … DROP CONSTRAINT` is a syntax error per ADR-0011's measurement).
- **`worktree` is the story's note, not a weakened column.** `''` is written literally by the INSERT (`repository.rs:28-31`) and asserted (`tests/session_repository.rs:238-241`).
- **`client_key` semantics are the ones HS-008 needs.** `UNIQUE` allows many `NULL`s, and `many_sessions_without_a_client_key_are_not_duplicates` proves three keyless creates coexist, while `the_same_client_key_twice_is_refused` proves the collision and that the original row is untouched.
- **The id is generated before the insert and is 16 lowercase hex.** `NEW_ID` at `repository.rs:21`, run at `repository.rs:52`, asserted for length and byte class at `tests/session_repository.rs:262-291`, and `id` is asserted to carry no column default (`:159`).
- **`set_status`'s `stopped_at` rule is exactly item 15's.** `let stopped_at = (status == Status::Stopped).then_some(now);` (`repository.rs:118`), the UPDATE touches only `status`, `status_reason`, `stopped_at` (`repository.rs:32-33`), `last_active_at`/`created_at` are proved unchanged (`tests/session_repository.rs:562-580`), and both directions are proved (`:530-560`). Because the set/clear is driven by the *new* status rather than by a transition, `stopped_at` is `Some` exactly while the status is `stopped` — a stricter invariant than the sentence, and the one `Session.stopped_at`'s doc states (`sessions/mod.rs:90-92`).
- **The corrupt-status path is an error naming the text, not a panic and not a coercion.** `row_to_session` returns `rusqlite::Result<Result<Session, RepoError>>` (`repository.rs:148-152`), `one()` transposes it (`repository.rs:129-136`), `create` returns the inner `Result` (`repository.rs:70-72`), and `list` collects with `?` before `rows.into_iter().collect()` (`repository.rs:93-108`) so a bad row aborts the read instead of being dropped. Proved end to end with raw SQL (`tests/session_repository.rs:362-395`). The one asymmetry — a *filtered* `list` never reads an unmatched row's status — is stated in ADR-0011 and in the test's own comment, so it is documented rather than accidental.
- **No statement escapes the repository.** Repo-wide grep for `(from|into|update|delete from) sessions` outside `src/` finds only the ADR prose, the repository itself, and the one deliberate raw `UPDATE` in `tests/` that the brief already accounts for.
- **Every acceptance box has a test that would fail if the behaviour were removed** (create/`starting`/`''`; read by id; read by `client_key`; list with and without a filter; update state incl. unknown id; the restart demo as a whole-struct `assert_eq!` at `tests/session_repository.rs:604-634`).

### Findings

#### P1 — should-fix: `NOT NULL` is asserted for only two of the nine non-nullable columns, so half the fragment's nullability is unproved.
`tests/session_repository.rs:145-153`:

```rust
assert!(column("worktree").2, "worktree must be NOT NULL");
assert!(column("status").2, "status must be NOT NULL");
…
for name in ["base_commit", "status_reason", "client_key", "stopped_at"] {
    assert!(!column(name).2, "{name} must be nullable");
}
```

The negative list proves the four nullable columns; nothing proves the other nine. Editing `network INTEGER NOT NULL DEFAULT 0` → `network INTEGER DEFAULT 0`, or dropping `NOT NULL` from `usage_in`/`usage_out`/`created_at`/`last_active_at`/`project`/`model`/`thinking`, leaves all 18 tests green — yet the acceptance box names `network (default 0)` and the spec fragment says `NOT NULL` on all of them, and the plan's acceptance table claims this pragma test proves it.
*Smallest fix:* replace the two `assert!` calls with a loop over the nine non-nullable names (`id` excepted, per the known decision) and keep the four-name nullable loop as the negative.

#### P2 — nit: `list`'s doc claims the ordering serves LRU eviction, but it orders the wrong way round.
`repository.rs:85-92`: "Ordered by `last_active_at` descending and then by id, so the order is total and the cap's LRU eviction (HS-020) has one defined first row." The first row is the *most* recently active; item 7/line 156 needs the least (`HolosmithSpec.md:156`). The descending order is right for the Sessions screen (HS-013), so the code is defensible and only the justification is off — and the same literal `"ORDER BY last_active_at DESC, id ASC"` appears twice in the function.
*Smallest fix:* hoist the clause to a `const ORDER` and reword to "newest activity first, for the list screen; the `sessions_last_active` index also serves eviction's ascending read."

#### P3 — nit: no test writes a non-`NULL` `base_commit`.
`new_session()` always passes `base_commit: None` (`tests/session_repository.rs:51-60`), so the `?5` binding in `INSERT` (`repository.rs:25-31`) and `base_commit: Option<String>`'s write-and-read-back are unproved — the only column with a value-carrying `Option` that no round-trip exercises.
*Smallest fix:* one assertion in `a_new_session_is_starting_in_an_empty_worktree`'s sibling, or extend `new_session` with a `base_commit` argument used by one test.

## Spec verdict: **OK with notes.** Every acceptance box, the note about `''`, and all ten invariants I was asked to check are implemented and proved; the one should-fix is a coverage gap in the migration test, not a wrong migration. The SQL-location guard covers `INSERT INTO`/`FROM`/`UPDATE`/`DELETE FROM` and *does* name the file and line, and it would fire on a plainly written planted statement in a new `src/` file — the weakness is that no test exercises that path (S2, Standards axis) and that four exotic spellings slip past it (S3).
## Resolution

No blocker. Three should-fix items and four nits fixed; one nit dismissed by deferring to the
reviewer's own judgement. Every new guard was proved to fire by mutating the code and watching the
test go red, not by asserting that the code compiles.

| Finding | Status | How / why |
| --- | --- | --- |
| **S1** no `case_*` was added to `scripts/verify.sh`, against HS-006's documented per-story rule | **fixed** | `case_sessions_table` greps the booted run's database bytes (`"$WORK"/data/app.db*`, so the WAL sidecars count) for `CREATE TABLE sessions` **and** the `sessions_status` index — the same byte-grep shape `scripts/check-static.sh` uses for `CREATE TABLE schema_version`, so no `sqlite3` binary is needed. One line in `serve_cases`; the run now reports 11 cases and 31 assertions. Proved load-bearing by renaming the index grep to something absent and watching `--no-build` fail `FAIL case_sessions_table/table`. This was the sharpest find in the review: HS-007 broke the rule HS-006 had written one story earlier, which is exactly what the convention exists to catch. |
| **S2** the SQL-location guard's *wiring* was unproven — the "planted statement" test only fed strings to the matcher, so emptying the walk left every test green | **fixed, and this is the same defect PER-36 found once already** | The walk is now `fn offenders_under(root: &Path) -> Vec<String>`, `no_sessions_sql_outside_the_repository` calls it on the real `src/`, and the planted test builds a `TempDir` and asserts the report equals exactly `["planted.rs:1: from sessions"]`. Five negative files prove the matcher discriminates rather than reporting everything: `let sessions = 1;`, `FROM sessions_notes`, a `\`-continuation that glues into `FROMsessions`, `ALTER TABLE old_sessions RENAME TO sessions`, and `CREATE INDEX sessions_status ON sessions`. Proved to fire by making the walk collect nothing and watching the planted test go red. |
| **S3** four spellings escaped the per-line literal match: `FROM main.sessions`, `FROM "sessions"`, `UPDATE OR REPLACE sessions`, and a statement split by a Rust `\`-continuation | **fixed** | The file text is normalised once before matching: `\`-continuations joined the way Rust joins them, whitespace runs collapsed, identifier quoting (`"`, `` ` ``, `[`, `]`) removed, the `main.` schema prefix dropped, and `or replace`/`or ignore` collapsed after `insert`/`update`. A per-character line map keeps the report pointing at the line the statement's first keyword is written on, so `file:line: phrase` still means what it says. One assertion per spelling is in the planted test. |
| **S4** `TempDir` is duplicated between `tests/session_repository.rs` and `tests/db_migrations.rs` | **dismissed** | Deferring to the reviewer's own judgement: "none required now; if a third copy appears, lift `TempDir` into `tests/support/mod.rs`". Lifting it now would move a helper out of the file whose style it documents for no behaviour change. The rule is recorded here so the third copy has an obvious home, and `tests/support/mod.rs` already carries the `#![allow(dead_code)]` rationale for exactly that. |
| **S5** `RepoError::Sqlite` printed a bare rusqlite string, so HS-008's operator would see `database is locked` with no operation attached | **fixed** | `RepoError::Sqlite { operation, source }` with a private `RepoError::sqlite(operation, error)` used at every public method, so `Display` is now `create: database is locked`. The blanket `From<rusqlite::Error>` is gone, so a future call site cannot silently produce an operationless error; `source()` still returns the wrapped error. A test asserts the `create: ` prefix. |
| **P1** `NOT NULL` was asserted for only `worktree` and `status`, so dropping it from any of the other seven left every test green | **fixed** | A loop now asserts all ten non-nullable columns (`project`, `model`, `thinking`, `worktree`, `status`, `network`, `created_at`, `last_active_at`, `usage_in`, `usage_out`), with `id` deliberately excluded because SQLite cannot express `NOT NULL` on a `TEXT PRIMARY KEY` (recorded in ADR-0011). The three `0` defaults are asserted too, since the acceptance names `network (default 0)`. |
| **P2** `list`'s doc claimed descending `last_active_at` gave eviction "one defined first row", but eviction needs the *least* recently active | **fixed** | The clause is a `const ORDER` (the literal appeared twice) and the doc now says what is true: newest activity first for the Sessions screen (HS-013), id making the order total, and the same `sessions_last_active` index serving eviction's ascending read (HS-020). Behaviour unchanged. |
| **P3** nothing wrote a non-`NULL` `base_commit`, so the only value-carrying `Option` had no round-trip coverage | **fixed** | `base_commit_round_trips_when_the_create_carries_one` creates with `Some("<40 hex>")`, asserts the returned value and re-reads it by id. |

After the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (138
lib, 20 config_cli, 12 db_migrations, 39 guards, 11 http_contract, 2 pin_single_source, 12
serve_cli, 19 session_repository, 4 version_cli — all green), `scripts/verify.sh` (exit 0, 11 cases +
the CORS sweep, 31 assertions), and the restart demo all pass.
