-- the host registry (schema 6). One row per machine this one knows about.
--
-- Spec item 4 gives the registration its checks ("the URL must be http(s), the host a MagicDNS name or an address
-- in 100.64.0.0/10, the port in an allowed range; redirects are not followed; and the `/api/health` reply must
-- carry the Holosmith name and version fields") and item 16 gives the seed its job ("each machine reads an
-- optional `hosts.toml` at startup and merges it into its hosts table (the file wins on conflicts)"). This is that
-- table.
--
-- `name` is the primary key because a name is the identity the operator picks and forwards with
-- (`/<host>/<id>/*`), so a second registration of one name is that machine registered again — `POST /api/hosts` is
-- an upsert on this key, and the merge's "the file wins" has one thing to compare. Two names may name one URL: an
-- alias is not a conflict, and resolving it would be inventing which of the two rows the operator meant.
--
-- `url` is an **origin** (`https://box.tail1234.ts.net`, `http://100.100.1.2:8080`) rather than a free URL: the
-- probe asks `GET /api/health` at it and nothing else, and appends the path it forwards. The column is TEXT
-- and the shape is `config::Origin`'s to refuse, as it already does for `control_origin` and `preview_origin`.
--
-- `reach` plus `last_error` is item 16's "a host that fails health is marked unreachable with the last error, not
-- removed" — **one row, two facts**. A probe *history* would be a second table with a retention question nothing
-- asks; the registry's job is "where can I send a request now", and the sentence is what the operator reads when
-- the answer is no. `version` is the health reply's own, kept because the probe has to read a Holosmith's version
-- to accept it at all and throwing it away would leave the next story re-asking.
--
-- `reach` is text rather than a CHECK constraint, for ADR-0011's reason: a corrupt row is an error at read time
-- that names the text it found, and the enum's words change without a migration that rewrites a constraint.
--
-- This migration is **additive**, and deliberately so (ADR-0047): an added table touches no table or column the
-- previous release reads, so `tests/schema_previous.rs`'s subset rule is unaffected and `docs/schema/previous.sql`
-- stays whatever the last release refreshed it to. Refreshing it is a release step (`scripts/schema-snapshot.sh`),
-- not a story's.
CREATE TABLE hosts (
  name TEXT PRIMARY KEY,
  url TEXT NOT NULL,
  reach TEXT NOT NULL,
  version TEXT,
  last_error TEXT,
  last_checked_at INTEGER NOT NULL
);
