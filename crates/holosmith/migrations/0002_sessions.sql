-- HS-007: the session row (schema 2). Everything the product does hangs off this table.
--
-- `status` has no CHECK constraint: the six values are an enum in Rust whose `FromStr` refuses
-- anything else, so a corrupt row is an error at read time naming the text it found, and HS-012's
-- transition rules arrive without a migration that has to rewrite a constraint (ADR-0011).
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
);

-- The only two queries that will not go by primary key: live sessions by status (the cap) and
-- the least recently active session (eviction). Both are HS-020's; adding them here is cheaper
-- than a migration later (ADR-0009).
CREATE INDEX sessions_status ON sessions (status);
CREATE INDEX sessions_last_active ON sessions (last_active_at);
