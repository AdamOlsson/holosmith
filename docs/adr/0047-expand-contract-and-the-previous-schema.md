# Expand/contract has two halves, and only one of them can be checked mechanically

Spec item 10 asks for migrations a symlink rollback can survive: "a release may add but never drop or rename
anything the previous release uses; cleanup ships one release later. That makes the symlink rollback safe without
restoring the database." Two of those clauses are about *one file* and one is about *two releases*, and this
story's job was to notice that the repository already had the first and none of the second.

## What ADR-0009 already built, and what it cannot see

`schema::contract_violations` (HS-004) refuses a `DROP TABLE`, an `ALTER TABLE … DROP COLUMN` or a `RENAME` in a
migration whose **slug** is not `contract`/`contract_*`, matched on words so that `renamed_at` and
`decontract_legacy` are not false positives. `cargo test` runs it over the embedded set, so the gate already
rejects a destructive migration that nobody named as one. That is the local half: it needs one file to answer, and
it is why a release that only expands cannot contain a cleanup by accident.

It cannot answer the other half, because it has never seen the previous release. To a single file, "drops
something an earlier release still uses" is unobservable — the file that *added* the column is still there and
still fine. So the second half is a fact about two schemas, and it needs a fact to compare against.

## The fact is a checked-in snapshot, and SQLite does the comparing

`docs/schema/previous.sql` is a real dump of the schema the previous release shipped — `CREATE TABLE` and
`CREATE INDEX`, taken from a database that release migrated. **It is not under `migrations/`**: that directory is
embedded with `include_dir!` and byte-grepped into the artifact by `scripts/check-static.sh`, so a snapshot there
would be applied as a migration and would break the build's own guard.

`tests/schema_previous.rs` executes it into one in-memory database, migrates another through `Database::open`, and
asserts every table and every column of the snapshot still exists — through `sqlite_master` and
`pragma_table_info`, not through a parser of this repository's own. That is deliberate: `src/sql.rs` is the only
SQL scanner here, and a second grammar over `CREATE TABLE` would be a second thing to be wrong about quoting,
constraints and comments.

The test is a **subset**, because adding is the point: a test that demanded equality would forbid the rule it
checks. It is guarded against passing vacuously in the two ways it could: the snapshot must name this schema's
tables (so a truncated or wrongly-sourced file fails), and the comparison is a function with its own tests over
synthetic schemas — a drop, a rename and an addition — so a comparison that stopped comparing cannot go green
(HS-061's lesson: a test's placement decides what it can prove).

**Measured:** a migration named `0005_contract_drop_usage_out.sql` that runs `ALTER TABLE sessions DROP COLUMN
usage_out` passes `contract_violations` (it is marked) and fails the subset test with
`this release dropped or renamed ["sessions.usage_out"], which the previous release still reads`. The same file
named `0005_drop_usage_out.sql` fails `the_embedded_migrations_carry_no_contract_violation` instead. The two halves
catch different mistakes, which is why both exist.

## The snapshot moves at release time, and its diff is the record

The mechanical rule can only be "a change to what the previous release used must be **deliberate**". It cannot be
"one release later": that is a fact about time and about which binary was promoted, and nothing in a repository
knows it. So a contract migration changes `docs/schema/previous.sql` in the same commit, and
`scripts/schema-snapshot.sh` is how the file is written. **Refreshing it is a release step** — you regenerate it
from the release you are leaving — which is why no story owns it and why the script's own header says so.

The consequence is the one worth having: the diff of that file *is* the record. A release that gives up a column
shows it in the snapshot, next to the contract migration that dropped it, and a reviewer sees both. The alternative
— an exemption list in the test — is how this kind of rule rots.

## A newer database is `serve`'s to survive and `doctor`'s to refuse

The acceptance's third bullet ("`doctor` reports schema version and refuses to start when the database is newer
than the binary") reads like a contradiction of item 10 until you separate the two moments. Continuing is what
makes a *rollback* safe: an older binary over a newer database must still serve, or switching `current` back needs
the restore that expand/contract exists to avoid. Refusing is what makes a *start* safe: an operator about to run
something must be told when the numbers disagree, and that is a check that runs before the service does.

So `schema::verdict(recorded, binary)` is one function with three answers and one sentence each, and the three
callers read it rather than comparing numbers themselves: `apply_pending` warns and continues, the banner prints
the **recorded** number, and `doctor` (HS-045) is where the `Newer` sentence becomes a refusal. `serve`'s behaviour
is unchanged; what changed is that the `warn` now carries the sentence, so the boot and the future refusal cannot
drift apart.

## Consequences

- A migration author has two ways to be stopped: an unmarked destructive statement (immediately, `cargo test`), and
  a marked one without a snapshot refresh (the subset test, with the instruction in the message).
- The gate grew a phase (`migrations`) so the rule is named in the output, and a case (`case_schema_newer`) that
  boots the shipped artifact over a database from schema 99 and asserts the banner's number, the warn's two
  versions and the word `doctor`, plus a 200 from `/api/health` — the "continues" half, measured.
- `scripts/schema-snapshot.sh` needs `python3` for its `sqlite3` module; the `sqlite3` CLI is not installed here and
  nothing in the gate may depend on it. It is a release-time tool, not a gate step.
