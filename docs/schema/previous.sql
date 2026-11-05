-- The schema the **previous** release shipped, as a fact a test can compare against (HS-075, ADR-0047).
--
-- This file is NOT a migration: it does not live in `crates/holosmith/migrations` (that directory is embedded
-- in the binary and byte-grepped by scripts/check-static.sh), and nothing applies it. `tests/schema_previous.rs`
-- runs it into a throwaway database and asserts that every table and column it names still exists in this
-- build's schema -- so a migration that drops or renames any of them fails the gate. A migration that *adds*
-- is invisible to it, because adding is the rule.
--
-- **Refresh it at release time**, from the release you are leaving, with
-- `scripts/schema-snapshot.sh <data root>/app.db`, and commit it with the change that needed it. A contract
-- migration (`NNNN_contract_*.sql`) changes this file in the same commit: that diff is the record of what was
-- given up, and of the release that gave it up.
--
-- The very first file of a project's life is that build's own schema, because there is no earlier release to
-- dump: that is this file's bootstrap case, and the provenance line below is what says which release it came
-- from.

-- Snapshot from a database of holosmith 0.1.0, schema 4 (scripts/schema-snapshot.sh).

CREATE TABLE events (
    session_id TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    type       TEXT NOT NULL,
    payload    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq)
);

CREATE TABLE schema_version (
    version    INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    applied_at INTEGER NOT NULL
);

CREATE TABLE sessions (
    id             TEXT PRIMARY KEY,           -- 16 lowercase hex characters, from SELECT lower(hex(randomblob(8)))
    project        TEXT NOT NULL,
    model          TEXT NOT NULL,
    thinking       TEXT NOT NULL,
    worktree       TEXT NOT NULL,              -- '' until the provisioner runs (HS-007's note)
    base_commit    TEXT,                       -- NULL until a worktree exists
    status         TEXT NOT NULL,              -- starting|idle|running|stopped|error|deleting
    status_reason  TEXT,
    client_key     TEXT UNIQUE,                -- idempotency key; NULL is not a key, and UNIQUE allows many
    network        INTEGER NOT NULL DEFAULT 0, -- internet access, read back as a bool
    created_at     INTEGER NOT NULL,
    last_active_at INTEGER NOT NULL,           -- LRU eviction (spec item 7, HS-020)
    stopped_at     INTEGER,                    -- set when the status becomes stopped, cleared on resume
    usage_in       INTEGER NOT NULL DEFAULT 0,
    usage_out      INTEGER NOT NULL DEFAULT 0
, last_seq INTEGER NOT NULL DEFAULT 0, expected_stop INTEGER NOT NULL DEFAULT 0);

CREATE INDEX sessions_last_active ON sessions (last_active_at);

CREATE INDEX sessions_status ON sessions (status);

