# The event log: one writer thread, a durable seq counter, and a broadcast after the commit

HS-009 is the first story with two writers. HS-008's create writes a `sessions` row behind
ADR-0012's `Arc<Mutex<Database>>` and its ADR said HS-009 "replaces the stopgap with the single
writer task". Reading the code rather than the sentence, that conflates two things: a *lock* that
makes writes serial, and a *task* that owns the event pipeline. Item 5 and V9 need the second. They
do not need a database actor that funnels every read and write through one task, and building one
would rewrite the create endpoint this story must not touch.

So this records four decisions made together: the mutex stays and a writer thread is added on top
of it, the seq counter is a column and not `MAX(seq)`, the broadcast happens strictly after the
commit, and the two SQL-location guards divide the tables between them. It also records what was
already true before this story — WAL and `synchronous=FULL` — so the acceptance bullet is re-proved
rather than re-implemented.

## Measured before deciding

- **`rusqlite::Connection` is blocking.** A 25 ms transaction on a tokio worker thread stalls every
  task that shares the runtime for those 25 ms. `tokio::sync::mpsc::Receiver::blocking_recv` and
  `try_recv` are available behind the `sync` feature the workspace already enables
  (`tokio-1.54.1/src/sync/mpsc/bounded.rs:424`), and `broadcast::Sender::send` is synchronous and
  callable from a plain thread. That is the whole reason the writer is a `std::thread`.
- **`Connection::unchecked_transaction(&self)` exists** (`rusqlite-0.37.0/src/transaction.rs:465`).
  A repository that borrows the database's one connection cannot take `&mut Connection` without
  handing the connection out, which ADR-0011 forbids; the mutex already makes the transaction
  exclusive in this process.
- **A composite primary key refuses a duplicate pair** with
  `UNIQUE constraint failed: events.session_id, events.seq`, the `ConstraintViolation` code. It
  also gives the read order: SQLite indexes it as `sqlite_autoindex_events`, so the range reads are
  index scans and no second index over the same columns exists. A named `events_session` index did,
  and only so `scripts/verify.sh` had a name to grep; it was a second b-tree write on every event
  row, on the story's durability path, and it is gone — the verify case greps
  `sqlite_autoindex_events`, which SQLite derives from the table name.
- **`ALTER TABLE … ADD COLUMN` rewrites the table's `sqlite_master` entry** to carry the new
  column, which is what lets `scripts/verify.sh` prove `sessions.last_seq` from the booted
  artifact's bytes instead of needing `sqlite3`.
- **The two SQL-location guards overlap on prose.** The sessions guard matches the literal phrase
  `update sessions` anywhere under `src/**`, so a counter bump written in
  `events/repository.rs` is reported as a sessions statement — and a doc sentence containing
  `from sessions` is reported too. Both were observed failing before they were fixed.
- **`broadcast::Sender::send` answers `Err` when there is no receiver.** A fresh process has no
  subscriber until HS-010, so that error is the normal path.

## The mutex stays; the writer is added on top of it

`AppState` keeps `database: Arc<Mutex<Database>>` and gains `events: EventWriter`. `serve` wraps
the database it already opens, starts the writer with a clone of the `Arc`, and hands the app the
sender. The writer takes the lock only for its own transaction and releases it before broadcasting.

- **HS-008 is untouched.** The create path, its 202/422/409 answers and its four `verify.sh` cases
  are byte-for-byte the ones that shipped; nothing about the event log changes how a session row is
  written.
- **"Single writer" is still true in the sense that matters:** exactly one thread assigns seqs and
  commits events, and SQLite sees one writer at a time. The lock is what serialises the create path
  against it.
- **A database actor is explicitly not this story's.** Funnelling every read and write through one
  task is a larger change with its own consequences, and no acceptance box needs it. A later story
  that wants one makes that decision and writes that ADR. This is ADR-0012's forward-looking
  sentence being resolved, not a silent change: the stopgap turned out to be the right shape for
  the writer to sit on.

The writer is a `std::thread`, not `tokio::spawn`: `blocking_recv` blocks, and a blocking task
would occupy a runtime worker for as long as the queue is empty. Its loop ends when every
`EventWriter` has been dropped, which is how a test — and process shutdown — stops it without a
flag. A `#[tokio::test]` observes that by waiting for `RecvError::Closed` on the stream, because
the broadcast sender is dropped with the thread.

## Seq is a column on `sessions`, never `MAX(seq)`

`events` has a composite primary key `(session_id, seq)` and the seq is `last_seq + 1`, where
`last_seq` is `sessions.last_seq`: bumped with `UPDATE … RETURNING` and read back **inside the
same transaction** as the insert.

`MAX(seq) + 1` would pass every test written today and break the first time HS-060 compacts:
compaction *deletes* rows, so the highest row can vanish and the next event would reuse a seq a
client has already seen — which the acceptance forbids in as many words ("never reused"). A column
cannot rewind because nothing deletes it. `seq_is_not_reused_after_the_highest_row_is_deleted`
deletes every event row for a session and asserts the next seq is 4, not 1; a `MAX(seq)`
implementation fails that test and nothing else in the suite.

The counter bump is `SessionRepository::bump_last_seq`, not a statement in the events repository,
because of the guards: a sessions statement written anywhere but `sessions/repository.rs` is
reported by the sessions guard, and "every statement lives in the repository that owns the table"
is the rule both guards exist to keep. The events repository runs it through its own transaction,
so the two repositories share a transaction without either owning the other's SQL.

## Commit, then broadcast

The writer collects a batch, commits it in one transaction, and only then calls `send` for each
event in seq order. A subscriber that reads the row back therefore always finds it.

`a_subscriber_sees_the_row_before_the_event` proves it deterministically rather than by racing the
commit: the test gives the writer a 500 ms window, appends, and asserts that a 100 ms timeout on the
stream yields nothing *and* that a second connection sees zero rows, then lets the window expire and
asserts the event arrives with its row already visible. A send moved before the commit fails the
first half; a commit after the send fails the second. Counting rows through the writer's own
connection, or through the repository after the fact, would pass either way — the test has to be a
different reader to mean anything, which is also why `one_batch_is_one_transaction` observes the
rows' absence from a second connection *while the batch is still open* rather than counting them
afterwards.

The 25 ms is a **latency ceiling**, not a batching rule. After the first event the writer waits out
the window, draining whatever arrives — including what arrived while it waited — and a batch of one
is still one transaction. Waiting rather
than committing the moment the queue looks empty is what makes "commits every 25 ms" true under one
event at a time; the interval is a `WriterOptions` field so a test can make it 5 ms or 300 ms.

All or nothing: one failed statement rolls the whole batch back and the writer logs the error and
drops it, so no client sees a seq the batch did not commit and no seq is consumed. That includes an
event naming a session that does not exist — there is no counter to bump, so the batch is refused
with `RepoError::UnknownSession` rather than inventing a seq.

**Superseded in part by ADR-0023.** HS-019's delete removes a session's row while its events may still
be queued, which makes "an event for a session that has gone" an ordinary race rather than a producer
bug — and refusing the batch for it would also discard every other session's event in the same 25 ms
window. So `insert_batch` now drops that one event and commits the rest; `RepoError::UnknownSession`
is gone with its last constructor. The all-or-nothing rule for every *other* failure stands.

## Bounded on both sides

Producers append over a bounded `mpsc(1024)`; a full queue slows the producer and never the writer.
Subscribers read a `broadcast(1024)`; `send` never waits for one, so a subscriber that never drains
cannot delay a commit — `a_lagged_subscriber_does_not_stall_the_writer` queues more events than the
buffer and asserts the writer committed every one of them while the subscriber was silent. That
subscriber is then told `Lagged(n)` and carries on; HS-010 re-fetches what it missed by last-seen
seq, which is why this story ships no `Last-Event-ID` handling of its own.

`EventWriter` keeps a clone of the broadcast sender and `subscribe()` returns a fresh `EventStream`,
so the subscriber HS-010 needs is made from the handle `AppState` already carries. Without it the
only stream would be the one `spawn` returned, `serve` drops that, and HS-010 would have to change
`spawn`'s signature to subscribe at all: the broadcast half was implemented and tested in-process
but unreachable in the shipped binary.

`send` returning `Err` because no receiver exists is normal and is not logged: a process with no
client connected is the ordinary state.

## WAL and `synchronous=FULL` were already proved

`Database::open` has set the four pragmas and read them back since HS-004 (ADR-0009), failing the
start when one did not take. This story re-proves the bullet on the file the writer uses rather than
re-asserting anything in production: `the_database_runs_wal_and_full_sync` opens the database and
reads `journal_mode` back through a second connection.

The two pragmas are not equally proved, and saying so is the point:

- `journal_mode` is a property of the file, so `wal` on a second connection is genuine proof that
  the database is in WAL; it outlives the connection that set it.
- `synchronous` is per-connection and SQLite's default is already `FULL(2)`, so a read-back of `2`
  proves only that the statement ran without an error and the value is now what we asked for — not
  that SQLite would have answered anything else if the statement had been skipped. A fresh
  connection re-asserting `2` proves nothing at all, which is why the test does not.

The startup's read-back is still worth having: it fails the start with `DbError::Pragma` when a
pragma did not take. `db.rs`'s `the_four_pragmas_read_back_on_a_real_file` makes the `synchronous`
half falsifiable by setting `PRAGMA synchronous=OFF`, reading `0` back, and only then applying the
configured pragmas and reading `2` — so the assertion reflects the statement rather than a default.

## Consequences

- The schema is 3. `SCHEMA_VERSION`, `version --json`, the startup banner, `tests/db_migrations.rs`'
  applied set, `tests/session_repository.rs`' column list and `scripts/verify.sh`'s
  `case_events_table` all follow from the one migration file.
- `sessions.last_seq` is `NOT NULL DEFAULT 0` and the last statement of the migration, so every
  pre-HS-009 row is correct without a backfill and nothing later in the file depends on the column
  being present.
- `events` has no foreign key to `sessions`. A seq without a counter cannot be assigned, so an
  event for a missing session is refused at write time; the constraint would add a cascade decision
  that session deletion (a later story) has to make, not this one.
- `Database::events()` and `EventRepository` mirror `Database::sessions()`: the connection never
  leaves the database, and the repository owns every statement and its own `src/**` guard.
- The two SQL-location guards are one helper in `src/sql.rs`, parameterised by
  `(table, statements, repository)`, so a fix to the matcher reaches both tables and HS-060's third
  table adds only its constants. Two copies had already drifted, and both named their scratch
  directory from the same label: in one crate that is one lib-test binary and one process id, so
  the events guard's `remove_dir_all` deleted the sessions guard's live fixture mid-run (PER-41
  S1/S2). `insert_batch` and `bump_last_seq` are `pub(crate)` for the same reason the guard exists —
  "exactly one writer assigns seq" is enforced by visibility, not by a comment.
- The writer thread is not joined at shutdown. Its loop ends when the last handle drops, and a
  process that exits with a batch in flight loses only uncommitted events — the ones no client was
  ever told about.
- **One event naming a missing session refuses the whole batch.** The insert is one transaction, so
  every valid event in the same 25 ms window — from any session — is discarded *after* `append`
  already returned `Ok`. Unreachable today because no producer emits an event; HS-011 is where
  per-event isolation or a pre-check has to be decided.
- **The `kill -9` demo is manual, not an automated test in this tree.** The plan promised one that
  drives the real binary, and the deliverable does not include it: the process-level crash run is
  HS-046's V9 check. HS-009's evidence is the in-process proofs — commit-before-broadcast on a
  second connection, one transaction per batch, the seq counter and the rows surviving a reopen —
  plus the manual demo. A process-level `kill -9` test here would be a second harness for a check
  that story owns.
- Not here, and not implied: the SSE stream and `Last-Event-ID` (HS-010), coalescing (HS-059),
  snapshots and compaction (HS-060), history truncation (HS-061), any producer (HS-011), a
  `GET …/events` route, a database actor, a new crate, or any change to the create endpoint.
