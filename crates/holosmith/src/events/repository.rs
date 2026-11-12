//! Every statement that reads or writes the `events` table.
//!
//! The repository borrows the open `Database`'s connection, exactly as `sessions` does, and this
//! file is the only one with an events statement. A test below walks `src/**` and fails if one
//! appears anywhere else — and, because the seq counter lives on `sessions`, that guard also
//! proves the bump is not written here: it is `SessionRepository::bump_last_seq`, run inside this
//! file's transaction, so each repository still owns its own table (ADR-0013).

use rusqlite::{Connection, Row, params};

use super::{
    Event, MESSAGE_DELTA, MESSAGE_END, MESSAGE_SNAPSHOT, NewEvent, RepoError, TURN_START,
    parse_delta,
};
use crate::sessions::repository::SessionRepository;

/// One row. `seq` was assigned by `bump_last_seq` inside this same transaction, so the row and
/// the counter commit together.
const INSERT: &str =
    "INSERT INTO events (session_id, seq, type, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5)";

/// The tokens one `message.end` reports, from its payload: `{"usage": {"in", "out"}}`.
///
/// The **only** kind whose payload this file looks inside, and it does so for one reason: a session's
/// spend is the sum of what its messages cost, and the writer's transaction is the one place where the
/// event's row and the session's tally can commit together (ADR-0013). Every other kind is
/// opaque here. A payload that carries no `usage`, or one that is not two non-negative integers, is a
/// message whose cost is unknown — reported as `None` rather than as zeros, because a backend that
/// cannot say what it spent must not look like one that spent nothing. The budget's own `None` handling
/// and the screen's sentence are what say so.
fn usage(payload: &str) -> Option<(u64, u64)> {
    let event: serde_json::Value = serde_json::from_str(payload).ok()?;
    let counts = event.get("usage")?;
    Some((counts.get("in")?.as_u64()?, counts.get("out")?.as_u64()?))
}

/// The read a client makes with its last-seen seq: one session, after `?2`, in order.
const SELECT_SINCE: &str = "SELECT session_id, seq, type, payload, created_at FROM events \
     WHERE session_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3";

/// The newest readable seq: `NULL` on an empty stream. This answers "what can a reader see",
/// which compaction can move backwards. The counter that must never move backwards is
/// `sessions.last_seq`, and seq assignment never reads this (ADR-0013).
const LATEST: &str = "SELECT max(seq) FROM events WHERE session_id = ?1";

/// The message-bearing rows of one session, **newest first**: what a truncated replay's cut is
/// computed from.
///
/// `message.delta` and `message.snapshot` are the two kinds that name a message and carry its text;
/// a message has at least one of them, and compaction replaces a run of the first with the second.
/// Descending order is what makes the scan a *window* rather than a pass: the caller stops as soon as
/// it has seen one message more than the window, so a long session costs its newest messages' rows and
/// not its whole log.
const MESSAGES_BACKWARDS: &str = "SELECT seq, payload FROM events \
     WHERE session_id = ?1 AND type IN (?2, ?3) ORDER BY seq DESC";

/// How many turn boundaries sit below a seq.
///
/// A truncated replay or a history page starts partway through the log, so the first `turn.start` it
/// carries is not turn 1. The client numbers a turn from the frames it has seen, so the count of the
/// boundaries it did *not* see is what keeps its labels true.
const TURNS_BELOW: &str = "SELECT count(*) FROM events \
     WHERE session_id = ?1 AND type = ?2 AND seq < ?3";

/// One page of history: the events below `?2`, **newest first**, so a full page is the rows nearest the
/// cursor rather than the oldest ones. The caller reverses it: a page is delivered in order.
const SELECT_BEFORE: &str = "SELECT session_id, seq, type, payload, created_at FROM events \
     WHERE session_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3";

/// Every event of one session: the delete flow's half of removing it.
const DELETE_LOG: &str = "DELETE FROM events WHERE session_id = ?1";

/// The `message.end` rows of one session that are past the cutoff, oldest first.
///
/// `type` and `created_at` are filtered in SQL rather than in Rust so a session that has nothing to
/// compact costs one index range scan and no payload parsing. **No `LIMIT` here**, and that is
/// load-bearing: the bound is applied *after* the already-folded filter in Rust, because a `LIMIT` before
/// it would keep re-reading the same oldest messages for ever once a session had more old messages than one
/// pass may fold — a limited pass would then never advance. The scan is over one session's seq range, which
/// the `LIMIT` never shortened anyway. `seq` is selected because it is what bounds the delta scan
/// below: a message's deltas all sit before its `message.end`, so the newest candidate's seq is the
/// furthest row a fold can possibly want.
const COMPLETED: &str = "SELECT seq, payload, created_at FROM events \
     WHERE session_id = ?1 AND type = ?2 AND created_at < ?3 ORDER BY seq ASC";

/// The message ids this session has already folded into a snapshot.
///
/// Read so an old message is not folded twice: its `message.end` row stays — compaction removes deltas
/// and nothing else — so without this a pass would keep re-reading a message whose text is already one
/// row.
const SNAPSHOTS: &str = "SELECT payload FROM events WHERE session_id = ?1 AND type = ?2";

/// The delta rows a fold reads, in seq order, below `?3`.
///
/// The bound is what keeps a pass cheap on a large session: without it this reads and parses every
/// delta row the session has ever held — including the ones inside the window, which the fold does not
/// want — while holding the database mutex the writer and every handler need. It is a *scan* bound and
/// not a delete bound: a delta above it is not folded, and one inside it that belongs to a message the
/// pass is not folding is filtered out by message id. It also narrows the strictness rule below: a delta
/// whose payload is not the pinned shape stops the session only when it sits inside this region, i.e.
/// when it could belong to a message being folded.
const DELTAS: &str = "SELECT seq, payload FROM events \
     WHERE session_id = ?1 AND type = ?2 AND seq < ?3 ORDER BY seq ASC";

/// Remove one row that a snapshot has replaced. One seq at a time, because the seqs are the fold's
/// own and an `IN` list of them would be a second spelling of what the read already answered.
const DELETE_ROW: &str = "DELETE FROM events WHERE session_id = ?1 AND seq = ?2";

/// Where a truncated replay starts, and how many turns closed before it.
///
/// `before_seq` is both the seq of the first event the replay carries and the value a client passes as
/// `?before=` for the next older page, because the history endpoint's cursor is exclusive. One number
/// for both is what lets the `history.truncated` frame and the page request agree without a second
/// field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truncation {
    pub before_seq: i64,
    pub turns_before: u32,
}

/// What one compaction pass did to one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Compaction {
    /// Messages whose delta rows became one snapshot.
    pub messages: usize,
    /// Delta rows the snapshots replaced.
    pub rows: usize,
    /// Delta rows whose payload is not the pinned shape. **Non-zero means the pass wrote nothing**:
    /// see [`EventRepository::compact`].
    pub unreadable: usize,
}

/// A snapshot's payload: the pinned three keys, in the contract's order (ADR-0018).
#[derive(Debug, serde::Serialize)]
struct Snapshot<'a> {
    message_id: &'a str,
    text: String,
    thinking: String,
}

/// One message's fold in progress: the two kinds accumulated separately, and the seqs that carried
/// them, in order.
#[derive(Default)]
struct Fold {
    text: String,
    thinking: String,
    seqs: Vec<i64>,
}

/// The `message_id` a `message.end` or `message.snapshot` payload names, or `None` when it names
/// none.
///
/// Deliberately weaker than [`parse_delta`]: compaction needs the id and nothing else from these two
/// kinds, and a stricter reading would refuse a payload whose only fault is a key it does not use.
fn message_id(payload: &str) -> Option<String> {
    let event: serde_json::Value = serde_json::from_str(payload).ok()?;
    Some(event.get("message_id")?.as_str()?.to_string())
}

/// The only way to read or write an `events` row. Obtained from `Database::events`.
pub struct EventRepository<'a> {
    connection: &'a Connection,
}

impl<'a> EventRepository<'a> {
    /// Borrow `connection`. `pub(crate)` rather than `pub`, so `Database::events` is the only way
    /// a caller gets one and the connection itself never leaves the database.
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    /// Insert `events` in **one** transaction and return them with the seqs it assigned.
    ///
    /// For each event the session's counter is bumped and read back, so the seq is `last_seq + 1`
    /// and never `MAX(seq) + 1`: compaction deletes rows, and a derived counter would rewind and
    /// reuse a seq. The counter bump is the sessions repository's statement, run through this
    /// transaction. All or nothing: a failure anywhere rolls the batch back, so no client ever
    /// sees a seq the batch did not commit.
    ///
    /// **An event whose session has gone is dropped, and the rest of the batch still commits**
    /// A row can vanish between a producer's append and this commit — the delete flow
    /// moves a session to `deleting` and then removes its row — and an event with no counter to
    /// bump is unrepresentable rather than an error. Refusing the batch for it would also drop
    /// every *other* session's event in the same window, which is the failure this rule prevents;
    /// the precedent is `SessionRepository::touch`'s "a session it can no longer touch has nothing
    /// to record".
    ///
    /// `pub(crate)` because the writer thread is the only caller: anything else inserting a row
    /// here could assign a seq outside the one writer, which is what "exactly one writer assigns
    /// seq" forbids (ADR-0013).
    pub(crate) fn insert_batch(&self, events: &[NewEvent]) -> Result<Vec<Event>, RepoError> {
        let sqlite = |error| RepoError::sqlite("insert_batch", error);
        // `unchecked_transaction` because the repository borrows the database's one connection
        // and cannot take `&mut` without handing the connection out. The writer holds the
        // database's mutex across this call, so nothing else in the process is writing.
        let transaction = self.connection.unchecked_transaction().map_err(sqlite)?;
        // One borrow for the whole batch: the counter bump and the tally are both statements against
        // the sessions table, run through this transaction.
        let sessions = SessionRepository::new(&transaction);
        let mut assigned = Vec::with_capacity(events.len());
        for event in events {
            let Some(seq) = sessions
                .bump_last_seq(&event.session_id)
                .map_err(RepoError::Sessions)?
            else {
                // The session was removed under this event: nothing can carry its seq, so it is
                // dropped. `debug`, not `warn`: this is an ordinary race with a
                // delete, not a producer bug.
                tracing::debug!(
                    target: "holosmith::events",
                    session = %event.session_id,
                    kind = %event.kind,
                    "an event was dropped because its session is gone"
                );
                continue;
            };
            transaction
                .execute(
                    INSERT,
                    params![
                        event.session_id,
                        seq,
                        event.kind,
                        event.payload,
                        event.created_at
                    ],
                )
                .map_err(sqlite)?;
            // The session's spend follows from its log, so the two commit together. After the
            // insert, so a batch that fails on an event leaves neither the row nor the tally behind.
            if event.kind == MESSAGE_END
                && let Some((input, output)) = usage(&event.payload)
            {
                sessions
                    .add_usage(&event.session_id, input, output)
                    .map_err(RepoError::Sessions)?;
            }
            assigned.push(Event::of(event, seq));
        }
        transaction.commit().map_err(sqlite)?;
        Ok(assigned)
    }

    /// Delete every event of one session, inside the caller's transaction.
    ///
    /// One statement and no transaction of its own: the delete flow removes the log and the
    /// session row together, so the transaction is
    /// [`SessionRepository::remove`](crate::sessions::repository::SessionRepository::remove)'s.
    /// It answers SQLite's own error rather than [`RepoError`] for that reason: the operation the
    /// caller reports is the removal, and this statement has exactly one way to fail.
    pub(crate) fn delete_session_log(&self, session_id: &str) -> Result<usize, rusqlite::Error> {
        self.connection.execute(DELETE_LOG, [session_id])
    }

    /// Up to `limit` events of one session, after `after`, in seq order — the page a client that
    /// reconnects with a last-seen seq reads.
    pub fn read_since(
        &self,
        session_id: &str,
        after: i64,
        limit: u32,
    ) -> Result<Vec<Event>, RepoError> {
        let sqlite = |error| RepoError::sqlite("read_since", error);
        let mut statement = self.connection.prepare(SELECT_SINCE).map_err(sqlite)?;
        let rows = statement
            .query_map(params![session_id, after, limit], row_to_event)
            .map_err(sqlite)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sqlite)
    }

    /// Where a replay that starts partway through the log begins, or `None` when the whole log fits in
    /// `window` messages (spec item 15).
    ///
    /// "The beginning of the 50th most recent message" is a **backwards** scan: the newest
    /// message-bearing rows are read in descending seq, and the distinct `message_id`s they name are
    /// collected until one more than `window` has been seen. The cut is then the **lowest** seq of the
    /// window's oldest message, so a block is always whole — cutting at a message's last row would
    /// render half of it. The scan stops the moment a row belongs to a message older than the window,
    /// so its cost is the newest `window` messages' rows, not the session's log.
    ///
    /// A row whose payload names no message id belongs to no block, so it neither opens a message nor
    /// ends the scan; compaction and the writer's coalescer read a payload the same forgiving way.
    ///
    /// `None` is "nothing was truncated", and it is decided by whether an older message was actually
    /// found — not by whether the window is full, because a log of exactly `window` messages has a cut
    /// at its own first row and its replay already starts at the beginning.
    pub fn truncation_point(
        &self,
        session_id: &str,
        window: u32,
    ) -> Result<Option<Truncation>, RepoError> {
        let sqlite = |error| RepoError::sqlite("truncation_point", error);
        let window = usize::try_from(window).unwrap_or(usize::MAX);
        let mut statement = self
            .connection
            .prepare(MESSAGES_BACKWARDS)
            .map_err(sqlite)?;
        let rows = statement
            .query_map(
                params![session_id, MESSAGE_DELTA, MESSAGE_SNAPSHOT],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(sqlite)?;
        let mut seen: Vec<String> = Vec::new();
        let mut cut: Option<i64> = None;
        let mut older = false;
        for row in rows {
            let (seq, payload) = row.map_err(sqlite)?;
            let Some(id) = message_id(&payload) else {
                continue;
            };
            if !seen.contains(&id) {
                if seen.len() == window {
                    // The first row of a message older than the window: the window is closed.
                    older = true;
                    break;
                }
                seen.push(id);
            }
            cut = Some(seq);
        }
        let (Some(before_seq), true) = (cut, older) else {
            return Ok(None);
        };
        Ok(Some(Truncation {
            before_seq,
            turns_before: self.turns_before(session_id, before_seq)?,
        }))
    }

    /// How many `turn.start` rows sit below `seq`.
    pub fn turns_before(&self, session_id: &str, seq: i64) -> Result<u32, RepoError> {
        let count: i64 = self
            .connection
            .query_row(TURNS_BELOW, params![session_id, TURN_START, seq], |row| {
                row.get(0)
            })
            .map_err(|error| RepoError::sqlite("turns_before", error))?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// One page of history: up to `limit` events of one session below `before`, **in seq order**
    ///
    ///
    /// The cursor is exclusive and the page is read newest-first so a full page is the rows nearest the
    /// cursor; the reversal is what makes the answer a page a client can prepend. Stability under
    /// concurrent writes is free: seq is strictly increasing, so an event committed after this read has
    /// a seq above every row the page could hold and cannot shift it. The one way a page's content moves
    /// is compaction removing rows (ADR-0032), which is the log's documented gap rule and not a cursor
    /// problem.
    pub fn read_before(
        &self,
        session_id: &str,
        before: i64,
        limit: u32,
    ) -> Result<Vec<Event>, RepoError> {
        let sqlite = |error| RepoError::sqlite("read_before", error);
        let mut statement = self.connection.prepare(SELECT_BEFORE).map_err(sqlite)?;
        let rows = statement
            .query_map(params![session_id, before, limit], row_to_event)
            .map_err(sqlite)?;
        let mut events = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sqlite)?;
        events.reverse();
        Ok(events)
    }

    /// The newest seq a reader can see, or `None` when the session has no events.
    ///
    /// Named for what it answers rather than for the counter: it is `max(seq)` over the rows that
    /// still exist, so the compaction moves it backwards, while `sessions.last_seq` is the
    /// counter that never does and that seq assignment reads (ADR-0013). No production caller
    /// today — it exists for the last-seen-seq reads and its tests.
    pub fn newest_visible_seq(&self, session_id: &str) -> Result<Option<i64>, RepoError> {
        self.connection
            .query_row(LATEST, [session_id], |row| row.get(0))
            .map_err(|error| RepoError::sqlite("newest_visible_seq", error))
    }

    /// Fold this session's old messages into one snapshot each (spec item 15).
    ///
    /// A message is eligible when it has a `message.end` row older than `cutoff`; it is folded when it
    /// still has delta rows and no snapshot already. The fold concatenates the `text` deltas and the
    /// `thinking` deltas separately, in seq order, and writes one `message.snapshot` at **the highest
    /// seq among the rows it removes** — the spec's own rule, which is why a seq may be *reused* here
    /// and nowhere else. `sessions.last_seq` is not read and not bumped: it is the counter that must
    /// never rewind, and compaction only ever removes seqs below the newest ones (ADR-0013).
    ///
    /// **One transaction for the whole session**, which is the story's "batched": the alternative is
    /// one transaction per message, and a client would then be able to read a log with some of a
    /// message's deltas gone and its snapshot not yet written. `limit` bounds how many messages one
    /// pass folds, so a long-neglected log costs a bounded transaction rather than an unbounded one; the
    /// next pass continues where this one stopped, which is why the bound is applied after the
    /// already-folded filter rather than in the query.
    ///
    /// **A delta whose payload is not the pinned shape stops the whole session.** Such a row names no
    /// message id, so it cannot be attributed, and folding the rows that can be attributed would delete
    /// text it cannot account for. The count is reported and nothing is written: compaction's contract is
    /// "without losing content", and a producer bug is the producer's to fix.
    pub fn compact(
        &self,
        session_id: &str,
        cutoff: i64,
        limit: u32,
    ) -> Result<Compaction, RepoError> {
        let sqlite = |error| RepoError::sqlite("compact", error);
        // `unchecked_transaction` for the reason `insert_batch` gives: the repository borrows the
        // database's one connection, and the caller holds the database's mutex across this call, so
        // nothing else in the process is writing.
        let transaction = self.connection.unchecked_transaction().map_err(sqlite)?;

        let folded = {
            let mut statement = transaction.prepare(SNAPSHOTS).map_err(sqlite)?;
            let rows = statement
                .query_map(params![session_id, MESSAGE_SNAPSHOT], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(sqlite)?;
            let mut folded = std::collections::HashSet::new();
            for payload in rows {
                if let Some(id) = message_id(&payload.map_err(sqlite)?) {
                    folded.insert(id);
                }
            }
            folded
        };

        // The candidates, oldest completion first, so the bound takes the messages that have waited
        // longest rather than an arbitrary set. `seq` is the message's `message.end`, which bounds the
        // delta scan.
        let mut candidates: Vec<(String, i64, i64)> = Vec::new();
        {
            let mut statement = transaction.prepare(COMPLETED).map_err(sqlite)?;
            let rows = statement
                .query_map(params![session_id, MESSAGE_END, cutoff], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .map_err(sqlite)?;
            for row in rows {
                let (end_seq, payload, completed_at) = row.map_err(sqlite)?;
                let Some(id) = message_id(&payload) else {
                    continue;
                };
                if !folded.contains(&id) {
                    candidates.push((id, completed_at, end_seq));
                }
            }
        }
        candidates.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        if candidates.is_empty() {
            return Ok(Compaction::default());
        }

        let wanted: std::collections::HashSet<&str> =
            candidates.iter().map(|(id, _, _)| id.as_str()).collect();
        // Every candidate's deltas sit before its own `message.end`, and the candidates are in seq
        // order, so the last one's end is the furthest row any of them can reach.
        let furthest = candidates
            .last()
            .map(|(_, _, seq)| *seq)
            .unwrap_or(i64::MAX);
        let mut folds: std::collections::HashMap<String, Fold> = std::collections::HashMap::new();
        let mut unreadable = 0;
        {
            let mut statement = transaction.prepare(DELTAS).map_err(sqlite)?;
            let rows = statement
                .query_map(params![session_id, MESSAGE_DELTA, furthest], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(sqlite)?;
            for row in rows {
                let (seq, payload) = row.map_err(sqlite)?;
                let Some(delta) = parse_delta(&payload) else {
                    unreadable += 1;
                    continue;
                };
                // A third kind is a payload the contract does not describe: folding it would have to
                // guess which node it belonged to.
                if !matches!(delta.kind.as_str(), "text" | "thinking") {
                    unreadable += 1;
                    continue;
                }
                if !wanted.contains(delta.message_id.as_str()) {
                    continue;
                }
                let fold = folds.entry(delta.message_id.clone()).or_default();
                if delta.kind == "text" {
                    fold.text.push_str(&delta.text);
                } else {
                    fold.thinking.push_str(&delta.text);
                }
                fold.seqs.push(seq);
            }
        }
        if unreadable > 0 {
            // Nothing has been written yet, and the transaction rolls back on drop.
            return Ok(Compaction {
                unreadable,
                ..Compaction::default()
            });
        }

        let mut compaction = Compaction::default();
        for (message_id, completed_at, _) in &candidates {
            let Some(fold) = folds.remove(message_id) else {
                // A completed message with no delta rows: already folded by an earlier pass, or a
                // message that never had any. Nothing to replace.
                continue;
            };
            let Some(&highest) = fold.seqs.last() else {
                continue;
            };
            for seq in &fold.seqs {
                transaction
                    .execute(DELETE_ROW, params![session_id, seq])
                    .map_err(sqlite)?;
            }
            let payload = serde_json::to_string(&Snapshot {
                message_id,
                text: fold.text,
                thinking: fold.thinking,
            })
            .expect("a snapshot's three fields serialise");
            transaction
                .execute(
                    INSERT,
                    params![session_id, highest, MESSAGE_SNAPSHOT, payload, completed_at],
                )
                .map_err(sqlite)?;
            compaction.messages += 1;
            compaction.rows += fold.seqs.len();
        }
        transaction.commit().map_err(sqlite)?;
        Ok(compaction)
    }
}

/// One row. Every column is `NOT NULL`, so the mapper cannot fail on a value.
fn row_to_event(row: &Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        session_id: row.get("session_id")?,
        seq: row.get("seq")?,
        kind: row.get("type")?,
        payload: row.get("payload")?,
        created_at: row.get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::db::{OpenOptions, apply_pending};
    use crate::schema;
    use crate::sessions::{NewSession, Session};
    use crate::sql::guard::Guard;

    /// The `created_at` and `applied_at` these tests write, so a timestamp is asserted rather
    /// than raced.
    const NOW: i64 = 1_760_000_000;
    /// The one file allowed to name the table in a statement.
    const REPOSITORY: &str = "events/repository.rs";

    /// The spellings an events statement can take, longest first so `DELETE FROM events` is
    /// reported as itself rather than as `FROM events`.
    const STATEMENTS: [&str; 4] = [
        "delete from events",
        "from events",
        "into events",
        "update events",
    ];

    /// This table's SQL-location guard: the walk and the matcher are shared with every other
    /// repository that has one (`crate::sql::guard`), so the two cannot drift or collide.
    fn guard() -> Guard {
        Guard::new("events", REPOSITORY, &STATEMENTS)
    }

    /// A migrated in-memory database, so a test reaches this repository without a writer.
    ///
    /// `insert_batch` is `pub(crate)` and the tests below are why it is still tested: the writer
    /// swallows a refused batch (it logs and drops it), so no integration test can observe the
    /// refusal — the seam has to be here.
    fn database() -> Connection {
        let mut connection = Connection::open_in_memory().expect("an in-memory database opens");
        let migrations = schema::embedded().expect("the embedded migrations are usable");
        apply_pending(
            &mut connection,
            Path::new(":memory:"),
            &migrations,
            OpenOptions::default(),
            NOW,
        )
        .expect("a fresh in-memory database migrates");
        connection
    }

    fn create_session(connection: &Connection, client_key: &str) -> Session {
        SessionRepository::new(connection)
            .create(
                &NewSession {
                    project: "holosmith".to_string(),
                    model: "claude-sonnet-4.5".to_string(),
                    thinking: "high".to_string(),
                    network: false,
                    client_key: Some(client_key.to_string()),
                    base_commit: None,
                },
                NOW,
            )
            .expect("a session is created")
    }

    fn new_event(session: &Session, kind: &str) -> NewEvent {
        NewEvent {
            session_id: session.id.clone(),
            kind: kind.to_string(),
            payload: format!("{{\"kind\":\"{kind}\"}}"),
            created_at: NOW,
        }
    }

    /// One `message.delta` of `message`.
    fn delta(session: &Session, message: &str, text: &str) -> NewEvent {
        NewEvent {
            session_id: session.id.clone(),
            kind: MESSAGE_DELTA.to_string(),
            payload: format!(
                "{{\"message_id\":\"{message}\",\"kind\":\"text\",\"text\":\"{text}\"}}"
            ),
            created_at: NOW,
        }
    }

    /// One turn's boundary, so `turns_before` has something to count.
    fn turn_start(session: &Session) -> NewEvent {
        NewEvent {
            session_id: session.id.clone(),
            kind: TURN_START.to_string(),
            payload: "{}".to_string(),
            created_at: NOW,
        }
    }

    #[test]
    fn no_events_sql_outside_the_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let guard = guard();
        assert!(
            crate::sql::guard::sources_under(&root)
                .iter()
                .any(|(file, _)| file == REPOSITORY),
            "the walk did not find {root:?}/{REPOSITORY}"
        );
        let offenders = guard.offenders_under(&root);
        assert!(
            offenders.is_empty(),
            "an events statement lives outside src/{REPOSITORY}, which owns every read and \
             write of the table: {offenders:?}"
        );
    }

    #[test]
    fn the_guard_fires_on_a_planted_statement() {
        // The guard above passes over today's tree, so this one plants a tree where it must fire.
        // The other files are the shapes the matcher has to leave alone; that the sessions
        // repository is one of them is what `no_events_sql_outside_the_repository` proves over
        // the real tree, which holds the counter bump.
        let planted = guard().temp_dir("planted");
        planted.plant("planted.rs", "let sql = \"SELECT seq FROM events\";\n");
        planted.plant("harmless.rs", "let events = 1;\n");
        planted.plant("suffix.rs", "let sql = \"SELECT * FROM events_notes\";\n");
        // A `\`-continuation after no space is not a break in Rust either: the two lines are the
        // one token `FROMevents` at runtime, which is not a statement.
        planted.plant(
            "glued.rs",
            "let sql = \"SELECT seq FROM\\\n     events\";\n",
        );
        planted.plant(
            "rename.rs",
            "let sql = \"ALTER TABLE old_events RENAME TO events\";\n",
        );
        planted.plant(
            "index.rs",
            "let sql = \"CREATE INDEX events_recorded ON events (created_at)\";\n",
        );
        assert_eq!(
            guard().offenders_under(planted.path()),
            ["planted.rs:1: from events"]
        );
    }

    #[test]
    fn the_guard_sees_through_the_spellings_that_hide_a_statement() {
        // Each spelling is one statement at runtime while the literal phrase in `STATEMENTS`
        // never appears on its line: a schema prefix, identifier quoting, a conflict clause and
        // a Rust `\`-continuation. One planted file each, so the reported line is the one the
        // statement was written on.
        let guard = guard();
        for (file, text, reported) in [
            (
                "prefix.rs",
                "let sql = \"SELECT seq FROM main.events\";\n",
                "prefix.rs:1: from events",
            ),
            (
                "quoted.rs",
                "let sql = \"SELECT seq FROM \\\"events\\\"\";\n",
                "quoted.rs:1: from events",
            ),
            (
                "bracketed.rs",
                "let sql = \"DELETE FROM [events]\";\n",
                "bracketed.rs:1: delete from events",
            ),
            (
                "conflict.rs",
                "let sql = \"UPDATE OR REPLACE events SET seq = 0\";\n",
                "conflict.rs:1: update events",
            ),
            (
                "continued.rs",
                "let sql = \"SELECT seq FROM \\\n     events\";\n",
                "continued.rs:1: from events",
            ),
        ] {
            let planted = guard.temp_dir("spelling");
            planted.plant(file, text);
            assert_eq!(guard.offenders_under(planted.path()), [reported], "{file}");
        }
    }

    #[test]
    fn a_windowed_log_is_cut_at_the_windows_oldest_message() {
        // Three messages of two delta rows each, in one batch, with a turn boundary in front of each.
        // The window of two keeps m2 and m3, so the cut is m2's **first** row and not its last: a block
        // is whole, and a cut at the last row would render half a message.
        let connection = database();
        let session = create_session(&connection, "truncation");
        let repository = EventRepository::new(&connection);
        let batch: Vec<NewEvent> = ["m1", "m2", "m3"]
            .into_iter()
            .flat_map(|message| {
                [
                    turn_start(&session),
                    delta(&session, message, "a"),
                    delta(&session, message, "b"),
                ]
            })
            .collect();
        repository
            .insert_batch(&batch)
            .expect("the batch is inserted");

        let cut = repository
            .truncation_point(&session.id, 2)
            .expect("the cut is computed")
            .expect("three messages do not fit a window of two");
        // m1 is rows 1-3, m2 is rows 4-6, m3 is rows 7-9; m2's first delta is seq 5.
        assert_eq!(cut.before_seq, 5, "the cut was not m2's first row");
        // Two turn boundaries sit below the cut, and that is the honest count: the cut is the message's
        // own first row, so the boundary that introduced m2 (seq 4) is above it and is replayed. Only
        // m1's (seq 1) and m2's are "before", which is what makes the client's first `turn.start`
        // — m3's — turn 3 rather than turn 1.
        assert_eq!(cut.turns_before, 2);

        // The same log with a window of three has nothing to truncate: the cut would be its own first
        // row, and the replay already starts there.
        assert_eq!(
            repository
                .truncation_point(&session.id, 3)
                .expect("the cut is computed"),
            None
        );
    }

    #[test]
    fn a_delta_that_names_no_message_neither_opens_a_message_nor_ends_the_scan() {
        // A producer bug is not a message: a row with no `message_id` cannot be attributed to a block,
        // so it must not consume a place in the window (which would cut too late) and must not stop the
        // scan (which would cut too early). One stray sits **between** the window's messages, which is
        // the only placement that tells those two bugs apart: a slot-consuming stray would cut at m3's
        // row, and a scan-ending one would answer `None`.
        let connection = database();
        let session = create_session(&connection, "unattributable");
        let repository = EventRepository::new(&connection);
        let stray = NewEvent {
            session_id: session.id.clone(),
            kind: MESSAGE_DELTA.to_string(),
            payload: "{\"text\":\"no id here\"}".to_string(),
            created_at: NOW,
        };
        let batch = vec![
            stray.clone(),
            delta(&session, "m1", "one"),
            delta(&session, "m2", "two"),
            stray,
            delta(&session, "m3", "three"),
        ];
        repository
            .insert_batch(&batch)
            .expect("the batch is inserted");

        let cut = repository
            .truncation_point(&session.id, 2)
            .expect("the cut is computed")
            .expect("m1 is older than the window's oldest message");
        // Seqs: stray 1, m1 2, m2 3, stray 4, m3 5. The window of two is m2 and m3, so the cut is m2's
        // row: a stray above it changed nothing, and one *between* the window's messages did not take a
        // place in it.
        assert_eq!(cut.before_seq, 3);
    }

    #[test]
    fn a_page_is_the_rows_below_the_cursor_in_order() {
        let connection = database();
        let session = create_session(&connection, "page");
        let repository = EventRepository::new(&connection);
        let batch: Vec<NewEvent> = ["a", "b", "c", "d"]
            .into_iter()
            .map(|kind| new_event(&session, kind))
            .collect();
        repository
            .insert_batch(&batch)
            .expect("the batch is inserted");

        let page = repository
            .read_before(&session.id, 4, 2)
            .expect("the page answers");
        // The two rows nearest the cursor, oldest first: a page a client can prepend.
        assert_eq!(
            page.iter().map(|event| event.seq).collect::<Vec<i64>>(),
            [2, 3]
        );
        assert!(
            repository
                .read_before(&session.id, 1, 2)
                .expect("the page answers")
                .is_empty(),
            "a cursor below every row answered a page"
        );
    }

    #[test]
    fn read_since_answers_after_the_given_seq_in_order() {
        let connection = database();
        let session = create_session(&connection, "read-since");
        let repository = EventRepository::new(&connection);
        let events: Vec<NewEvent> = ["first", "second", "third"]
            .into_iter()
            .map(|kind| new_event(&session, kind))
            .collect();
        let assigned = repository
            .insert_batch(&events)
            .expect("the batch is inserted");
        assert_eq!(
            assigned.iter().map(|event| event.seq).collect::<Vec<i64>>(),
            [1, 2, 3]
        );

        let after = repository
            .read_since(&session.id, 1, 10)
            .expect("the page answers");
        assert_eq!(
            after.iter().map(|event| event.seq).collect::<Vec<i64>>(),
            [2, 3]
        );
        let limited = repository
            .read_since(&session.id, 1, 1)
            .expect("the page answers");
        assert_eq!(
            limited.iter().map(|event| event.seq).collect::<Vec<i64>>(),
            [2]
        );
        assert_eq!(
            repository
                .newest_visible_seq(&session.id)
                .expect("the newest visible seq answers"),
            Some(3)
        );
    }

    #[test]
    fn an_event_for_a_gone_session_is_dropped_and_the_rest_of_the_batch_commits() {
        // a delete can remove a row between an append and this commit. The event has no
        // counter to bump, so it is unrepresentable — and refusing the batch would throw away the
        // *other* sessions' events that share the same 25 ms window.
        let connection = database();
        let kept = create_session(&connection, "batch-kept");
        let gone = create_session(&connection, "batch-gone");
        let repository = EventRepository::new(&connection);
        assert!(
            SessionRepository::new(&connection)
                .remove(&gone.id)
                .expect("the row is removed"),
            "the session was there to remove"
        );

        let assigned = repository
            .insert_batch(&[
                new_event(&gone, "session.status"),
                new_event(&kept, "session.status"),
                new_event(&gone, "turn.start"),
            ])
            .expect("the batch commits");
        assert_eq!(
            assigned
                .iter()
                .map(|event| (event.session_id.as_str(), event.seq))
                .collect::<Vec<(&str, i64)>>(),
            [(kept.id.as_str(), 1)],
            "only the live session's event was assigned a seq"
        );
        assert!(
            repository
                .read_since(&gone.id, 0, 10)
                .expect("the read answers")
                .is_empty(),
            "a removed session has no log"
        );
    }

    #[test]
    fn a_batch_of_nothing_but_gone_sessions_still_commits() {
        // The empty case has its own assertion because an early `Err` would pass the test above
        // for the wrong reason (a half-committed batch would look like a drop).
        let connection = database();
        let gone = create_session(&connection, "only-gone");
        SessionRepository::new(&connection)
            .remove(&gone.id)
            .expect("the row is removed");
        let assigned = EventRepository::new(&connection)
            .insert_batch(&[new_event(&gone, "turn.end")])
            .expect("the batch commits nothing rather than failing");
        assert!(assigned.is_empty(), "{assigned:?}");
    }
}
