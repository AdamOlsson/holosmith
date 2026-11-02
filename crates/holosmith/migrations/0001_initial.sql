-- HS-004: schema 1 is "the migration system exists". Every table a later story owns arrives
-- in its own numbered migration (ADR-0009); nothing else is created here.
CREATE TABLE schema_version (
    version    INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    applied_at INTEGER NOT NULL
);
