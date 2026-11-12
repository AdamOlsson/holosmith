-- the machine's Web Push subscriptions (schema 5). One row per endpoint a browser registered.
--
-- Spec item 11 names it: "Endpoints: `POST /api/push/subscribe` and `DELETE /api/push/subscribe`, table
-- `push_subscriptions`", with three things earning a notification — "a turn finished after running longer than 20
-- seconds; `session.status` error; a budget or timeout stop" — and one row per subscribed device.
-- The **endpoint is the primary key** because that is the identifier a push service mints: a browser produces
-- exactly one endpoint per subscription, and a second `POST` of the same endpoint is that browser re-subscribing
-- (its keys may have rotated). A synthetic id would let the same browser register twice and receive every
-- notification twice, which no client could tell from a bug in this build.
--
-- `p256dh` and `auth` are **rows here and not files under `secrets/`**, unlike the machine's own keys
-- (ADR-0050's `keys.toml` and 's `vapid.toml`). Those two are this machine's identity: written once by a
-- deploy, never rewritten, and mounted nowhere. A subscription is the opposite in every respect — it arrives over
-- HTTP at any moment, there is one per browser rather than one per machine, and a phone that re-subscribes
-- overwrites its own row. A file per subscription would be a directory of rename-written files nothing reads as a
-- set, and the "every subscribed device" query is the one read item 11 names.
--
-- This migration is **additive**, and deliberately so (ADR-0047): an added table touches no table or column the
-- previous release reads, so `tests/schema_previous.rs`'s subset rule is unaffected and
-- `docs/schema/previous.sql` stays whatever the last release refreshed it to. Refreshing it is a release step
-- (`scripts/schema-snapshot.sh`), not a story's.
CREATE TABLE push_subscriptions (
  endpoint TEXT PRIMARY KEY,
  p256dh TEXT NOT NULL,
  auth TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
