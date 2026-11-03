-- HS-009: the event log (schema 3). Every durable record a session produces is one row here.
--
-- The composite primary key is the story's "unique on (session_id, seq)" and the read order at
-- once: SQLite indexes it as `sqlite_autoindex_events`, so a range read for one session is an
-- index scan and no second index over the same columns is needed — that would cost a second
-- b-tree write on every event row, on the durability path (PER-41 S4). `type` and `payload` are
-- the wire names; the payload is JSON held as text, so the schema never has to change when an
-- event's shape does.
CREATE TABLE events (
    session_id TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    type       TEXT NOT NULL,
    payload    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq)
);

-- The seq counter, and the whole reason it is a column and not `MAX(seq)`: HS-060's compaction
-- deletes event rows, so a counter derived from the rows would rewind and reuse a seq. Bumped in
-- the same transaction as the insert (ADR-0013). `NOT NULL DEFAULT 0` makes every existing row
-- correct without a backfill, and an ADD COLUMN is the expand step ADR-0009 asks for. It is the
-- last statement in this file: nothing after it can be read as depending on the new column.
ALTER TABLE sessions ADD COLUMN last_seq INTEGER NOT NULL DEFAULT 0;
