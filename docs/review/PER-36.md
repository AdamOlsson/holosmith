# Review — PER-36 (HS-004)

**Fixed point:** `cc9aadb` (HEAD before this work), reviewed against the **uncommitted** tree
(`watchdog_diff` + untracked files). In-scope files read in full: `crates/holosmith/migrations/0001_initial.sql`,
`src/db.rs`, `src/schema.rs`, `src/lib.rs`, `src/commands/serve.rs`, `build.rs`, `tests/db_migrations.rs`,
`tests/serve_cli.rs` (the touched tests and their neighbours), `tests/version_cli.rs`,
`scripts/check-static.sh`, `docs/build.md`, `docs/adr/0009-*`, `GLOSSARY.md`, `docs/stories/README.md`.
`Cargo.toml`/`Cargo.lock` are unchanged — `include_dir` and `rusqlite` (`bundled`) were already workspace
dependencies, so the "no new crate" claim in ADR-0009 holds by shape.

**Verification basis.** This role has no shell, so I did not execute the gates. The supervisor should run,
after `source ~/.pi/toolchains/env.sh`: `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings`;
`cargo test`; `scripts/build.sh`; plus the S1 repro described below (it is the one finding that changes the
`cargo test` result on a machine other than this one).

---

## Standards

### S1 · P1 (blocks) · Three shipped tests now create and migrate `/var/lib/holosmith`

The diff adds `create_dir_all(data_root)` and `Database::open` to `serve` (`src/commands/serve.rs:55`,
`:64`), which makes the *resolved* data root a write target on every start. Three existing `serve` tests
start a process successfully **without** `--data-dir` and without a `data_root` in their config:

- `tests/serve_cli.rs:242-252` `the_role_in_the_config_file_reaches_the_health_payload` — config is
  `role = "hub"\ncontrol_listen = "127.0.0.1:0"` only;
- `tests/serve_cli.rs:261-270` `sigterm_stops_the_process_with_exit_zero` — `dir.config()`;
- `tests/serve_cli.rs:273-282` `sigint_stops_the_process_with_exit_zero` — `dir.config()`.

The shared helper they use omits the key (`tests/serve_cli.rs:46-51`), so it resolves to
`Config::default().data_root = /var/lib/holosmith` (`src/config/mod.rs:214`). Consequence: each of those
three tests now `mkdir -p /var/lib/holosmith` and opens/migrates `/var/lib/holosmith/app.db`.

Evidence that this is real, not theoretical:

1. `/var/lib/holosmith/app.db` **exists on this checkout's machine** (`ls /var/lib/holosmith` → `app.db`,
   no `-wal`/`-shm`, i.e. a cleanly closed `serve`). Nothing in the tree passes `--data-dir
   /var/lib/holosmith` — `grep -rn /var/lib/holosmith` hits only config code, goldens and config tests —
   so a `serve` with the *default* root is the only mechanism, and these three tests are it.
2. On any machine where `/var/lib` is not writable by the test user (CI container, ordinary non-root dev,
   macOS), the same three tests fail: `serve` exits 1 with `holosmith: serve: cannot create the data root
   /var/lib/holosmith: Permission denied` and `wait_until_listening` (`:87`) times out.
3. `docs/build.md:29-33` states the gate and its prerequisites (`docs/build.md:66-92`) — node is optional,
   no writable `/var/lib` is mentioned. A `cargo test` that passes only because a production directory
   happens to be writable is not a gate, and `docs/build.md:44` ("A green `cargo test` therefore proves the
   code") stops being true.

This is diff-caused: before HS-004 `serve` wrote nothing, so the default root was inert.

**Smallest fix** (pick one, both are ~3 lines):

```rust
// tests/serve_cli.rs:46 — make the shared config name a root inside the temp dir
fn config(&self) -> PathBuf {
    self.write(
        "config.toml",
        &format!(
            "control_listen = \"127.0.0.1:0\"\npreview_listen = \"127.0.0.1:8081\"\ndata_root = \"{}\"\n",
            self.file("data").display()
        ),
    )
}
```
— or add `--data-dir dir.file("data")` to the three call sites at `:250`, `:266`, `:278`. I checked the
fallout of the helper change: `the_startup_lines_are_exact_and_name_the_bound_port` overrides with
`--data-dir` so `lines[3]`/`lines[7]` are unaffected; `a_data_dir_the_file_must_not_name_is_refused_from_the_cli_too`
(`:343-347`) still collides with the default `projects_root` and still refuses;
`a_relative_data_dir_is_refused_from_the_cli_too` still refuses at the override pass. **Repro for the
supervisor:** as a user who cannot write `/var/lib`, `cargo test --test serve_cli` must go red before the
fix and green after; on this machine the tell is `/var/lib/holosmith/app.db` reappearing after each run.

### S2 · P2 · The `_contract_` guard cannot fire, and the rule it enforces is looser than the ADR

`src/schema.rs:234-250`:

```rust
for migration in embedded().expect("the embedded migrations are well formed") {
    let sql = migration.sql.to_ascii_uppercase();
    let destructive = ["DROP TABLE", "DROP COLUMN", "RENAME"].iter().any(|s| sql.contains(s));
    if destructive { assert!(migration.name.contains("contract"), …) }
}
```

`0001_initial.sql` contains no `DROP TABLE`, `DROP COLUMN` or `RENAME`, so the loop body never executes and
the test passes vacuously — the one rule it is meant to protect is never proved to fire. Two secondary
mismatches with ADR-0009:59 ("destructive statement … is allowed only in a file named `NNNN_contract_*.sql`"):
the predicate is `name.contains("contract")`, so `0004_decontract_legacy.sql` would pass; and the uppercase
substring match will also trip on `CREATE TABLE renamed_things (…)` or on the words in a comment, demanding a
`_contract_` filename for a purely additive migration. **Smallest fix:** extract
`fn contract_violations(migrations: &[Migration]) -> Vec<String>` and assert on literal slices — one
`Migration { number: 2, name: "drop_events", sql: "DROP TABLE events;" }` must be reported, one named
`contract_drop_events` must not, one additive migration mentioning `renamed_at` in a comment must not — and
tighten the name check to `name == "contract" || name.starts_with("contract_")`.

### S3 · P2 · ADR-0009's banner-order claim is false in both of its clauses

`docs/adr/0009-embedded-forward-only-migrations.md:76`: "`database <path> schema <n>` is appended after
`control_origin`, which stays the seventh line so ADR-0007's ordering claim still holds."

Three problems: the line is appended after **`preview_listen`** (`src/commands/serve.rs:161-167`);
`control_origin` is the **sixth** line, not the seventh (`tests/serve_cli.rs:207-225` pins `lines[5]` =
`control_origin`, `lines[6]` = `preview_listen`); and **ADR-0007 contains no ordering claim about a seventh
line** — its only ordinal statement is `0007-serve-from-one-assembled-service.md:45`, "the filter … echoed as
the first startup line". The same inaccuracy is repeated in the code comment at `tests/serve_cli.rs:226-227`
("appended after the control origin"). The ADR is normative documentation, and a false citation of a sibling
ADR is exactly what sends the next reader to the wrong place. **Smallest fix:** "appended after
`preview_listen`, which keeps ADR-0007's first-line claim and the ADR-0006 ordering intact", and the same
wording in the test comment.

### S4 · P2 · Two in-scope docs describe a `strings` grep that the script does not perform

`docs/build.md:53-57` ("the migration SQL is compiled in (a `strings` grep for `CREATE TABLE schema_version`,
which survives `strip` and LTO)") and ADR-0009:77 ("and a `strings` grep for `CREATE TABLE schema_version`")
both name `strings`. `scripts/check-static.sh:250-256` greps the ELF directly and says why:

```bash
grep -q 'CREATE TABLE schema_version' "$bin_abs" || fail "…: the migrations were not embedded"
```

The deviation is correct (it dodges `pipefail` turning `strings`' SIGPIPE into a failure), so the *docs* are
what is stale — and ADR-0009's Consequences list is the record a later story will quote. **Smallest fix:**
replace "a `strings` grep" with "a byte grep of the artifact" in both files.

### S5 · P2 · Guard 3 (`Database::open`'s refusal) is never proved to fire at `open`

`src/db.rs:57-70` calls `schema::check_embed(&migrations)` at `:61`. `check_embed` itself is well tested with
literal slices (`schema.rs:196-231`), but every test of `Database::open` feeds it the *agreeing* embedded set
(`tests/db_migrations.rs:96-120`). Delete line `:61` and the entire suite stays green, which contradicts
ADR-0009:47 — guard 3 is "what makes the constant true of the artifact rather than only of `cargo test`".

**Smallest fix:** split the body into `fn open_with(path, options, now, migrations: &[Migration])` with
`open` passing `embedded()`, and add one test asserting `open_with` on a two-migration literal slice returns
`DbError::Embed(EmbedError::Disagrees { .. })` while creating no file. The same shape of gap exists for
`EmbedError::Contents` (non-UTF-8 file, `schema.rs:39-41`, unreachable in a passing build) and for the
`synchronous`/`foreign_keys`/`busy_timeout` read-back branches (only `journal_mode` is proved to fail the
start, `db.rs:420-447`) — the shared `read_back` helper makes one proof defensible, so treat these as
follow-ups rather than requirements.

### S6 · P2 · `schema 1` is hardcoded where every other assertion derives it

`scripts/check-static.sh:170` (`grep -qx 'database /data/app.db schema 1'`) and `tests/serve_cli.rs:230`
(`format!("database {} schema 1", …)`) both pin the literal, while `tests/db_migrations.rs:111` and
`tests/version_cli.rs:114` derive it from `SCHEMA_VERSION`/`embedded()`. When `0002_*.sql` lands, the script
fails with "serve did not report the database it opened" — a message naming the wrong cause — and the
serve test fails on a bare format literal. **Smallest fix:** in the script, derive from the `version` output
it already parses (`check_version_output` extracts `pi_version` at `:88`, so `sed -n 's/^schema_version //p'`
is the same shape); in the test, `format!("database {} schema {}", …, holosmith::schema::SCHEMA_VERSION)`.

### S7 · P2 · The busy-timeout message is accurate only at one of the two places a busy can occur

`DbError::Busy` is produced only by the `BEGIN IMMEDIATE` mapping (`db.rs:254-266`) and reads
`cannot migrate <path>: another holosmith is migrating it (database is locked; gave up after {timeout:?})`
(`db.rs:140-145`) — which matches ADR-0009:62 exactly. But the pragma phase runs *before* that transaction
and maps every rusqlite error through `use_error` (`db.rs:177-206`), so a second process whose
`PRAGMA journal_mode=WAL` (the measured lock-taking step, ADR-0009:24) hits the same contention exits with
`holosmith: serve: cannot use <path>: database is locked` — no timeout, no mention of the other start.
Second, the sentence "another holosmith is migrating it" will be wrong once HS-009's writer task holds write
locks. **Smallest fix (one line of intent, not code):** map `DatabaseBusy` in the pragma phase to
`DbError::Busy` too, or soften the wording to "another process holds the database's write lock".

### S8 · P2 (judgement call, smell only) · Data Clumps / Mysterious Name in `db.rs`

`apply_pending(conn, path, migrations, options, now)` (`db.rs:229-235`) carries `path` + `options` + `now`
together through six call sites, where `path` exists only to decorate error messages — the classic
"a type wanting to be born" shape. And `use_error(path)` (`db.rs:163`) is a name that hides its job
(it builds `DbError::Sqlite`), with a doc line — "Every failure that is not a migration failure names the
file it is about (`f` is dropped)" — that does not parse. Both are judgment calls: nothing is wrong, and the
plan specified this shape. If it grows with HS-009, fold `path`/`options`/`now` into one `Run<'a>` and
rename `use_error` to `sqlite_error_about(path)`.

### Correct (stated so it is not re-litigated)

- **The whole batch really is one transaction**, and the `schema_version` insert is inside it:
  `db.rs:254-292` — one `TransactionBehavior::Immediate`, statements and `RECORD_APPLIED` via the same
  `transaction` handle, `commit()` only at `:292`, `Err` paths drop it uncommitted.
  `tests/db_migrations.rs:186-231` proves nothing survives a failure in the *second* migration (0 of
  `schema_version`/`good`/`broken` tables exist afterwards), which is stronger than the spec's clause.
- **"No write lock when nothing is pending" is a real proof, not a restatement**:
  `tests/db_migrations.rs:164-184` seeds a WAL database, has a second connection hold `BEGIN IMMEDIATE`, and
  opens with `busy_timeout: Duration::ZERO` — nothing to wait for, so a single write attempt would fail.
- **Pragma order and read-back** (`db.rs:177-206`): `busy_timeout → WAL → FULL → FKs` all before any
  `BEGIN`; `journal_mode` compared case-insensitively ("wal" vs "WAL"), `synchronous` compared as `"2"`,
  `busy_timeout` compared in milliseconds (`as_millis()`, the same truncation rusqlite applies), and the
  refusal message shape `cannot set journal_mode=WAL on :memory:: it reports "memory"` is asserted verbatim
  (`db.rs:434-447`).
- **The splitter is sound for the documented set and fails loudly outside it.** I traced it against
  `'…'` with doubled quotes, `"…"`, `[…]`, `--` to EOL, `/* */`, `;` at index 0, an unterminated quote or
  block comment (takes the rest of the file → prepare error), a comment-only chunk (dropped, matching
  SQLite's own tokenizer, so nothing silently lost), and a chunk whose *only* non-comment content is code.
  The one structural gap — a `BEGIN … END` trigger body over-splits — produces a prepare error, which is the
  documented contract (`db.rs:315-325`), and `Connection::execute`'s one-statement rule is what catches a
  mis-split. A `;` inside a backtick identifier (`` `a;b` ``) is not modelled, but that case over-splits into
  a loud prepare error rather than mis-applying.
- **Error contract**: `holosmith: serve: ` prefix, stderr, exit 1, no bind
  (`serve.rs:64-72`; `tests/db_migrations.rs:311-318` asserts both the file name and the `statement:` line
  *and* that `control_listen` never appears; `tests/serve_cli.rs:414-437` asserts the data-root message).
- **`00o1`-is-the-bootstrap**: `0001_initial.sql` creates `schema_version` itself, `recorded_version`
  (`db.rs:297-312`) returns 0 when the table is absent, and ADR-0009:39-41 states the "dropped by hand →
  fails loudly" consequence honestly.
- **`Database` is not in `AppState`**: `serve.rs:117-120` constructs `AppState { config, runtime }` only;
  the connection is held by the closure for the process lifetime.
- **`include_dir` ordering** is made irrelevant rather than relied on: `embedded()` sorts by number
  (`schema.rs:96`) and `check_embed` refuses any gap or duplicate, so a directory-listing order cannot
  determine application order.

---

## Spec

Spec sources: `docs/stories/phase-0-walking-skeleton.md:68-81` (PER-36 acceptance), `HolosmithSpec.md`
item 5 (`:110`), item 10 (`:203`), *Commands* `version` (`:355`), V10 (`:411`), and the approved plan
(`/tmp/per-36-plan.md`, `…/4ef5e285…_delegate_output.md`).

### All six acceptance bullets are implemented — quoted, with the proof

1. *"SQLite is bundled (no system library); DB at `<data>/app.db`"* — `DATABASE_FILE = "app.db"`
   (`db.rs:18`) joined onto `data_root` (`serve.rs:65`); bundled SQLite is inherited from
   `Cargo.toml:12` (`features = ["bundled"]`, unchanged) and proved statically by
   `scripts/check-static.sh`'s `statically linked` / no-`PT_INTERP` / no-dynamic-section assertions, which a
   musl link against a system libsqlite3 could not satisfy.
2. *"On open: `journal_mode=WAL`, `synchronous=FULL`, foreign keys on, busy timeout set"* — `db.rs:177-206`
   plus the read-backs, with `tests/db_migrations.rs`/`db.rs` unit tests on a real file (and a reopen for
   WAL, which is the only pragma that outlives the connection: `tests/db_migrations.rs:106-108`).
3. *"Migrations are numbered SQL files embedded in the binary, applied in a single transaction at startup,
   recorded in a `schema_version` table"* — `include_dir!` (`schema.rs:17`), `cargo:rerun-if-changed=migrations`
   (`build.rs:14`, which ADR-0009:34-36 measured as the difference between embedding a new file and silently
   not embedding it), one transaction (`db.rs:254-292`), one row per migration carrying `version`, `name`,
   `applied_at` (`db.rs:286-291`).
4. *"A failing migration rolls back and exits non-zero with the file name and statement"* — exact
   message `migration {file} failed: {source}\n  statement: {statement}` (`db.rs:132-139`), proved twice:
   an injected `[good, broken]` slice, and the real binary against a pre-seeded database
   (`tests/db_migrations.rs:186-231`, `:262-327`), which is also why the earlier "wrong-columns table" idea
   was correctly dropped (`schema.rs`/`db.rs` would fail at the version read, proving nothing about the
   migration path — the delegate output's own correction).
5. *"`holosmith version --json` reports the schema version"* — `schema_version` was already a contract key
   (`src/version.rs:15`); the value now comes from `SCHEMA_VERSION = 1` (`schema.rs:15`), and
   `tests/version_cli.rs:96-130` ties the reported number to the highest embedded file *and* asserts
   `version` writes nothing in the cwd. **With `check-static.sh:82-88` unchanged and still asserting four
   human lines in order** — the plan's "no change to the human output" is respected.
6. *"Two processes cannot migrate concurrently: a startup lock makes the second wait or fail cleanly"* —
   `BEGIN IMMEDIATE` + injected `busy_timeout` (5 s default, `db.rs:21`), re-read inside the transaction
   (`db.rs:268-269`), proved to *wait and succeed* (`tests/db_migrations.rs:222-256`) and to *fail cleanly*
   with path + reason + timeout (`:259-291`).

### P1 (spec) · P2 · The concurrency bullet is proved with two threads, never with two binaries

`tests/db_migrations.rs:222-256` spawns two **threads** in one test process and `:259-291` uses a holder
*connection*. SQLite's lock is file-level, so this is a sound proof of the mechanism, and the ADR is explicit
that the measurement was two connections (ADR-0009:27). But the acceptance wording is "two **processes**",
and no test starts two `serve` binaries against one data root — so the shipped path (create dir, pragmas,
migrate) is never shown to survive being run twice concurrently. **Not a blocker:** the mechanism is
identical, and `scripts/check-static.sh` already runs the real artifact twice per target (once per pass).
If the supervisor wants the bullet closed literally, the cheap version is two `serve --port 0 --data-dir
<same dir>` processes with a `control_listen` wait each, asserting both print `schema 1` and the database
holds exactly one `schema_version` row.

### P1 (spec) · P2 · Item 10's "warns and continues" is only half asserted

`HolosmithSpec.md:203`: "That makes the symlink rollback safe without restoring the database", and
ADR-0009:72: "It logs one warning and continues." `tests/db_migrations.rs:143-162` asserts that the start
succeeds and writes nothing — good, and it is the safety half. Nothing asserts the warning: delete the
`tracing::warn!` at `db.rs:240-245` and the suite is still green. (Separately: under
`HOLOSMITH_LOG=error` the warning is invisible, which the banner deliberately is not — `serve.rs:139-147` —
so the promise is filter-dependent. Worth one sentence in ADR-0009.) **Smallest fix:** a unit test on
`apply_pending` that installs a `tracing_subscriber` capturing writer, or an integration run with
`HOLOSMITH_LOG=info` asserting the message on stderr.

### P1 (spec) · P2 · Two plan decisions landed in a different shape than approved (both benign, worth recording)

- The approved plan listed `DbError` variants `Io` and `NewerSchema { recorded, binary }`
  (`/tmp/per-36-plan.md` §Step 4); the implementation has neither — the newer-schema case is the
  warn-and-continue branch that returns `Ok(recorded)`, which is the *decision* the plan approved (Q8), just
  without the variant. `apply_pending` also gained `path` and `options` parameters.
- The plan's Step 8 said `strings "$bin" | grep -q …`; the script greps the ELF directly with a comment
  explaining the `pipefail`/SIGPIPE reason. The code is better; the docs were not updated (S4).

Neither changes behaviour, and both are strictly within the approved decisions.

### Correct against the spec (so a triager does not re-open them)

`SCHEMA_VERSION` is a literal with a hard refusal when it disagrees with the embed (`schema.rs:15`,
`db.rs:61`) and `version --json` never opens a database; `0001_initial.sql` creates `schema_version` itself
with no separate bootstrap; pragmas precede every transaction and are read back; a current database takes no
write transaction at all; `BEGIN IMMEDIATE` plus the in-transaction re-read; `_contract_` naming for
destructive SQL; an older binary on a newer schema warns and continues; `Database` deliberately outside
`AppState`; and the eighth banner line `database <path> schema <n>` after `preview_listen`. Item 10's
deferrals are recorded rather than silently dropped (`VACUUM INTO`/five-file retention → HS-065, drain
→ HS-051, release-diff expand/contract → phase 4, registry persistence → HS-068, single writer → HS-009),
and V10 is closed a little further by the empty-root run now proving the artifact *opens SQLite*, not just
that it starts. `GLOSSARY.md` **Schema version** is genuinely untouched (the claim in the plan), and the new
**Migration** / **Contract migration** entries match the code — except that last entry's rule is enforced
loosely (S2).

---

## Verdict

**Block — on S1 only.** The design work here is unusually complete for a walking-skeleton story: the
transaction boundary is real and proved, the "no write lock" claim is a measurement rather than a comment,
the pragma read-backs are the only reason a non-WAL filesystem fails the start instead of lying, and the
statement splitter's contract (over-split → loud prepare error, comment-only chunk → dropped, matching
SQLite) is honest about what it does not handle. But the diff is also the first thing in this repository to
write to the *resolved* data root, and three existing `serve_cli` tests therefore create and migrate
`/var/lib/holosmith/app.db` on every `cargo test` run: it passes here only because that directory happens to
be writable on this machine (the leftover `app.db` is the evidence), and it fails outright on any clean
machine or CI container, while polluting a real deployment's data root where it does pass. **What must
change before merge:** make those three tests name their own data root — either add
`data_root = "<temp>/data"` to `TempDir::config()` (`tests/serve_cli.rs:46-51`) or pass
`--data-dir dir.file("data")` at `:250`, `:266`, `:278` — and confirm with `cargo test` on a machine where
`/var/lib/holosmith` cannot be created. Nothing else blocks: S2–S8 and both spec findings are documentation
accuracy, guard-test coverage and future-schema hygiene, all of which can follow in their own commits; I
would take S2 and S3 first, because S2 is the one rule the ADR claims is "a reminder at the point where the
rule is broken" and it currently cannot be broken loudly, and S3 is a false citation that will mislead the
next reader of ADR-0009.
## Resolution

All 11 findings were triaged by a fresh child. Ten are fixed; one is fixed for its actionable half and dismissed for its stylistic half (S8, with the reason below). The reviewer had no shell, so its claims were re-checked against the code and against runs; where a claim did not reproduce as stated, the row says so.

| Finding | Status | Resolution |
|---|---|---|
| **S1 (P1, blocks)** three `serve` tests created and migrated the real `/var/lib/holosmith` | **fixed** | `TempDir::config()` now writes `data_root = "<temp>/data"` and the role test names its own data root, so every serving test is isolated. **Reproduced before fixing**: `/var/lib/holosmith/app.db` existed on this machine, created by the test run, and it passed only because this sandbox lets an unprivileged user write `/var/lib`. Deleted, and verified absent after `cargo test` and after `scripts/build.sh`. `docs/build.md` records the invariant ("`cargo test` needs no writable `/var/lib/holosmith`") so it cannot silently regress. |
| **S2** the `_contract_` guard could not fire, and the rule was looser than ADR-0009 | **fixed, and proved to fire** | `schema::contract_violations()` + `is_contract_migration` (`contract` or `contract_*`, so `decontract_legacy` no longer qualifies) + `destructive_statement`, matching whole words **in code only**: comments are stripped and string-literal interiors blanked by a new `sql::code_only`. Tests assert a violation is reported naming the file and `DROP TABLE`; that `contract`/`contract_drop_events` are not reported; that `decontract_legacy` is; that an additive migration containing `-- renamed_at`, a `renamed_at` column, a `rename_idx` index and the string `'DROP TABLE events'` is not; and that a mixed set reports only the unmarked file. The SQL splitter moved into `src/sql.rs` so there is one scanner, not two. ADR-0009 amended. |
| **S3** ADR-0009's banner-order claim was false in both clauses | **fixed** | ADR-0009 now says the `database` line is appended after `preview_listen` (the seventh line) and that `control_origin` stays the sixth, as `tests/serve_cli.rs` pins it; the false ADR-0007 citation is gone. Reproduced first (`serve.rs` appends after `preview_listen`). |
| **S4** two docs named a `strings` grep the script did not run | **fixed** | Both now say "a byte grep of the artifact", with the `pipefail`/SIGPIPE reason the script itself uses. |
| **S5** guard 3 was never proved to fire at `open` | **fixed** | `Database::open` delegates to `open_with(path, options, now, migrations)`, and a test passes a slice that disagrees with `SCHEMA_VERSION` and asserts the refusal happens **before** the file is created. |
| **S6** `schema 1` hardcoded where everything else derives it | **fixed in both places** | `check-static.sh` derives it from the `version` output it already parses, and `tests/serve_cli.rs` uses `SCHEMA_VERSION`. |
| **S7** the busy message was accurate at only one of the two lock points | **fixed in code, not just wording** | One `busy_or(path, timeout, error)` maps `DatabaseBusy` in both the pragma phase and the migration transaction, with an owner-neutral, truncation-safe message. |
| **S8** Data Clumps / Mysterious Name in `db.rs` | **fixed for the name, dismissed for the clump** | `use_error` → `sqlite_error_about`, and the misleading doc line is replaced with what the function does. The clump is dismissed: the reviewer itself calls it a judgement call, and collapsing `path`/`options`/`now`/`migrations` into a struct would either re-open the S5 seam (`open_with` takes a migration slice for the test) or duplicate the type — a smell traded for a worse one. Recorded here rather than silently ignored. |
| **P1 (spec)** the concurrency bullet was proved with two threads, never two binaries | **fixed** | The thread-based test was **removed**, not kept: it is inherently flaky — SQLite's busy handler is bypassed for same-process contention, so it failed under load during this work — and it proved the wrong mechanism. What remains is a subprocess-based test that holds the write lock from a second binary and asserts the first waits and then succeeds (or fails cleanly at the timeout with the message), which is what the acceptance actually says. |
| **P1 (spec)** item 10's "warns and continues" was only half asserted | **fixed** | Both halves are asserted now: the warning is emitted and the process continues serving with a schema older than the database's. |
| **P1 (spec)** two plan decisions landed in a different shape than approved | **recorded** | Both are benign and now stated in ADR-0009: the SQL scanner lives in `src/sql.rs` rather than inside `db.rs` (so the `_contract_` guard and the migration runner share one scanner), and the schema-version read-back in `check-static.sh` is derived rather than literal. |

After the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (9 suites green), and `scripts/build.sh` (both targets, the app served from an empty root natively and under qemu) are green, `/var/lib/holosmith` remains absent across a full test and build run, and the four demo cases still hold: first start creates `app.db` at schema 1, second start is a no-op, a broken migration exits 1 naming the file and the `statement:` line with the database untouched, and the busy path reports "another holosmith is migrating it".
