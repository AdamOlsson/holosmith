# The binary opens, asserts and migrates its own database

This is the first story that writes anything durable, and it is the story that decides what every later story may do to the schema. Three things had to be settled together: how migrations are embedded and recorded, how they are applied and how two processes are kept from applying them at once, and what a later release is allowed to change. The spec constrains all three — item 10 wants forward-only numbered SQL files embedded in the binary, applied at startup in a transaction, following expand/contract so that the `current` symlink can be rolled back without restoring the database; item 5 wants `journal_mode=WAL` and `synchronous=FULL`; *Single binary and CLI* wants `version` to report a schema version and *Build and release* wants SQLite bundled.

## Measured before deciding

Measured on the build machine against `rusqlite` 0.37 / SQLite 3.50.2 with the `bundled` feature the workspace already declares.

- **Pragmas read back.** `pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))` answers `"wal"`; `synchronous` then reads `2`, `foreign_keys` `1`, and `busy_timeout` the milliseconds it was given.
- **WAL cannot be entered from inside a transaction, and the refusal is silent.** A fresh delete-mode database, `BEGIN IMMEDIATE`, then `journal_mode=WAL` → `Ok("delete")`. No error, no warning. The pragma has to run before any `BEGIN`, and the value has to be read back: a filesystem that cannot do WAL answers `"delete"` the same way.
- **An in-memory database cannot be tested for WAL.** `journal_mode=WAL` on `:memory:` answers `Ok("memory")`. Every pragma test needs a temporary file. The `-wal` and `-shm` files exist only while the connection is open and has written, and are gone after a clean close.
- **A lock held by another connection in the same process is not waited for.** Two threads opening one database both reach schema 1 most of the time, but under load the loser's `journal_mode=WAL` answers `database is locked` immediately — no wait, whatever the busy timeout — because the unix VFS reports a same-process conflict rather than deadlocking on it. Two *processes* do wait. That is why the concurrency proof is two `serve` processes on one data root, which is what the acceptance says anyway, and why two threads are not a stand-in.
- **`BEGIN IMMEDIATE` plus `busy_timeout` is a usable lock.** With another connection holding the write lock and `busy_timeout=0`, the second `BEGIN IMMEDIATE` fails in 29 µs with `database is locked`, `ErrorCode::DatabaseBusy`. With `busy_timeout=300 ms` and a holder that keeps the lock for 200 ms, the second waiter *succeeded* at 200 ms. Waiting and succeeding is the normal case; the timeout is the exception.
- **DDL rolls back.** A failing statement inside an `IMMEDIATE` transaction, with the transaction dropped rather than committed, leaves zero of the tables it created.
- **Only prepare-time errors carry the SQL text.** rusqlite 0.37's `Error::SqlInputError { msg, offset, sql }` fires for a syntax error and for `table … already exists`; `no such table`, `duplicate column name`, `NOT NULL constraint failed` and `UNIQUE constraint failed` are plain `SqliteFailure` with no SQL at all. So "report the statement that failed" cannot lean on the error, because the errors that matter most in a migration are the ones that carry nothing.
- **`Connection::execute` is a one-statement primitive.** It refuses a chunk holding two statements (`MultipleStatement`), fails loudly if the chunk is a trigger body split in half (`no such table: main.trg`), and *rejects* a comment-only chunk with `bad parameter or other API misuse` — a splitter has to skip those.
- **`include_dir!("$CARGO_MANIFEST_DIR/migrations")` works, and a new file is invisible without a build-script line.** With `cargo:rerun-if-changed=migrations` in `build.rs`, adding `0003_events.sql` re-expands the macro and the file is embedded. Without it, the macro is not re-run and the new migration is silently *not* in the binary while the source tree says it is.

## The database is opened, configured and asserted by the binary

`serve` creates `data_root` (`mkdir -p`; ADR-0006's handoff says `serve` creates no directories, so this story owns it), then opens `<data>/app.db` **after** the config is loaded and **before** the listener binds, so a database that cannot be opened exits 1 before any port exists. The order inside `Database::open` is fixed:

`busy_timeout` → `journal_mode=WAL` → `synchronous=FULL` → `foreign_keys=ON` → read all four back → migrate.

The busy timeout comes first because the WAL conversion may need a lock; every pragma comes before any transaction because of the silent refusal measured above. The read-back is not ceremony: it is the only way to know that the durability settings the spec requires are actually in force, and it fails the start — naming the pragma and the value it received — when `journal_mode` is not `wal`, `synchronous` is not `2`, `foreign_keys` is not `1`, or the busy timeout is not the one asked for. Unit tests therefore use a temporary file and also reopen it to prove the settings survive; an in-memory database is not a stand-in.

The database is opened for every role, `hub` included: there is one binary and one schema, and a hub that could not open its own data root would be a machine with a broken deploy. A writable data root is consequently a deploy requirement.

## `schema_version` belongs to the schema

The spec names the table and not its columns. It is `version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL`, one row per applied migration, so the current version is `max(version)` and the applied set is a query rather than a number. `applied_at` is unix seconds taken from a clock passed into the function that writes the rows, so a test can assert the value.

There is no separate bootstrap, and no chicken-and-egg: **`0001_initial.sql` creates `schema_version` itself**, and "the table is absent" means "this database is empty, apply everything from 1". The migrator reads the table only when it exists; a database whose `schema_version` was dropped by hand therefore reads as empty, every migration is re-applied, and the first `CREATE TABLE` fails loudly instead of the process pretending the schema is current. Schema 1 consequently means "the migration system exists" — which is the honest description of a walking-skeleton story, and it keeps ADR-0006's rule that no story pre-declares a table whose columns another story owns.

`PRAGMA user_version` was measured as the alternative pointer (it defaults to `0`) and rejected: an integer cannot record *which* migrations were applied, has no name and no timestamp, and the spec already names a `schema_version` table.

## Forward-only SQL, embedded at compile time, with four guards

Migrations live in `crates/holosmith/migrations/NNNN_slug.sql`, four-digit zero-padded numbers, `^[0-9]{4}_[a-z0-9_]+\.sql$`, embedded with `include_dir!` (already a dependency for the web app, so no new crate and no code generation). `SCHEMA_VERSION` stays a literal `u32` in `src/schema.rs`, because `version --json` must answer without opening a database and a literal is what makes that obviously true.

Four guards keep the literal and the directory from drifting apart:

1. A naming test: every embedded file matches the pattern, the numbers are unique, and they run 1..=n with no gap.
2. An agreement test: the highest embedded number equals `SCHEMA_VERSION`.
3. A hard refusal at `Database::open` when the two disagree, because guard 2 only runs under `cargo test` while the stale embed is a property of the *artifact*. Measured above: without `cargo:rerun-if-changed=migrations`, adding a file leaves it out of the binary and nothing else notices.
4. That `cargo:rerun-if-changed=migrations` line in `build.rs`.

Rejected: generating the constant in `build.rs`. It would make drift impossible, but it buys that with a second place that parses migration file names, and it turns `SCHEMA_VERSION` from a literal into an environment string that has to be parsed — for a value whose only consumers are `version` and one assertion.

## One transaction per start, and SQLite's own lock as the startup lock

All pending migrations are applied in **one** `BEGIN IMMEDIATE` … `COMMIT`, so a failure at the third rolls back the first and second as well; that is what "applied at startup in a transaction" means and it is what makes "a broken migration leaves the database untouched" true rather than nearly true.

The same transaction is the concurrency lock. The migrator reads `max(version)` without a lock, and if nothing is pending it stops there — a start on a current database takes **no write lock at all**, which is what makes the second start a no-op rather than a queue. If something is pending it opens `BEGIN IMMEDIATE`, **re-reads the version inside the transaction**, and applies only what is still missing, so a process that waited for another process's migration finds nothing to do instead of applying it twice. `busy_timeout` is 5 s, a constant in `src/db.rs`, injected through `OpenOptions { busy_timeout }` so tests can use 100 ms — the same shape as `ServeOptions::shutdown_grace` in ADR-0007. The lock can be met in two places — the WAL conversion, which runs before any transaction, and the migration's `BEGIN IMMEDIATE` — so both map a `DatabaseBusy` to one error, and neither says "busy" about a specific owner: when the timeout expires the start fails with exit 1 and `holosmith: serve: cannot open <path>: another process holds the database's write lock (database is locked; gave up after 5s)`. The wording has to survive, whose writer task will hold write locks that have nothing to do with migrating.

Rejected: an advisory `flock` on a sidecar file (needs `libc` or `unsafe`, which the workspace denies, and adds a lock whose lifetime is not the transaction's), and a lock table (a second thing to keep correct, with no property SQLite's own lock lacks). Also rejected: one transaction per migration — it would leave a half-migrated database behind on failure, which is the state the demo says must not exist.

## A failed migration names its file and its statement

Because the interesting errors carry no SQL text, the runner splits each file into statements itself — a small splitter that understands `'…'`, `"…"`, `[…]`, `--` and `/* */` and skips chunks that hold only comments — and executes each chunk with `Connection::execute`, which is what catches the splitter's own mistakes: an under-split chunk is refused as `MultipleStatement` and a trigger body cut in half fails as a syntax error. The error is then one line naming the file and the SQLite message, and a second line with the statement:

```
holosmith: serve: migration 0002_broken.sql failed: no such table: sessions
  statement: CREATE INDEX sessions_by_status ON sessions(status)
```

The runner takes `&[Migration]`, so the embedded set is one caller and a test can pass a literal slice — no test-only feature, no temporary migrations directory, and no risk that the tested path is not the shipped path. Rollback is proved with an injected slice (`[good, broken]`: nothing from either survives); the exit code, the file name and the statement are proved against the real binary by pre-seeding a database whose `schema_version` holds a `version = 0` row, which makes the real `0001_initial.sql` collide with the table it creates.

## What a later release may do to the schema

Expand/contract is the rule (item 10): a release may add but never drop or rename anything the previous release uses, and the cleanup ships one release later. Today the only enforceable half is the naming: a destructive statement (`DROP TABLE`, `DROP COLUMN`, `RENAME`) is allowed only in a file named `NNNN_contract.sql` or `NNNN_contract_*.sql`, and a test over the embedded SQL fails on one found anywhere else. The predicate reads the file the way SQLite does — comments removed, the insides of string literals blanked, and keywords matched as whole words — so `-- renamed_at` in a comment and a table called `renamed_things` are not destructive statements, while `ALTER TABLE … DROP COLUMN` is; and the test asserts on literal slices, so the rule is proved to fire instead of passing over today's single migration. That is a reminder at the point where the rule is broken, not the enforcement — the real check compares this release's migrations with the previous release's, and it belongs to the promotion machinery of phase 4.

The other direction is a *rollback*: an older binary meeting a database whose recorded version is higher than its own `SCHEMA_VERSION`. It logs one warning and continues; the warning is a `tracing` event, so it follows `HOLOSMITH_LOG` and is invisible under a filter that excludes `warn`, which is why the startup banner — not the log — is what proves the start continued. Refusing would contradict item 10's promise that switching `current` back needs no restore, and expand/contract is precisely what makes continuing safe.

## Consequences

- The startup banner grows to eight lines: `database <path> schema <n>` is appended after `preview_listen` (the seventh line), so the preceding seven keep their order and `control_origin` stays the sixth line, as `tests/serve_cli.rs` pins it. ADR-0007:27 calls the origin "the seventh startup line"; that ordinal was already off by one before this story and ADR-0001–0008 are not touched here, but ADR-0007's other claim — the log filter is echoed as the **first** startup line — still holds. `tests/serve_cli.rs` moves from seven lines to eight.
- `scripts/check-static.sh` currently asserts that `serve` writes **nothing** under `--data-dir`. That flips: exactly `app.db`, plus `app.db-wal` while the process is running, and a byte grep of the artifact for `CREATE TABLE schema_version` proving the SQL is in the shipped artifact (string literals survive `strip` and LTO — measured on the current artifact). The grep reads the ELF directly rather than piping `strings` into `grep -q`: under `pipefail` the SIGPIPE `strings` dies of is a failure.
- `Database` is held by `serve` for the process lifetime and is deliberately **not** in `AppState`. Sharing one connection between handlers is the single-writer, group-commit decision of item 5, and that decision belongs. Pre-deciding it here would create a second seam nobody reviewed; a later change owns it and may change it.
- A hub needs a writable data root, because every role opens the database. Whether the host registry is persisted there is the decision.
- Deferred: `VACUUM INTO` backups and the five-file retention are that story's; drain before a migration is that story's; the release-diff half of expand/contract is phase 4's.
- A new migration needs no build-script edit, but it does need the `rerun-if-changed` line to exist — which is why guard 3 refuses to start rather than trusting the build.
