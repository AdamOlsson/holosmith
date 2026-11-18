# A tool's key is a second credential table in the one key store

A session's agent needs the Linear MCP API key, and the machine had nowhere to put it. `<data>/secrets/keys.toml`
held two tables: `[providers]`, the model-provider keys the key proxy swaps in, and `[sessions]`, the dummy key each
live session spends. `holosmith key set linear` was refused — "linear is not a provider this build knows:
opencode-go, anthropic, …" — because `key::PROVIDERS` is model providers and only model providers.

So the key went into each session's own `pi-home/mcp.json`. That file is **inside a session mount**, which is the one
place this build's promise says a real key is not: the agent can read it, a rotation is an edit of every session's
copy, and the dummy-key machinery that exists for model providers — the session holds a throwaway, the machine holds
the real one — does not apply to it at all. The measurement was not a failure of the MCP integration; it was the key
store having one vocabulary where the machine has two.

## D1 — a second table in the same file, and a second fixed table in `key.rs`

`Secrets` gains `tools`, rendered as `[tools]` between `[providers]` and `[sessions]` — and only while it holds a
key (see **Rollback**) — and `key.rs` gains `Tool { id, host, auth, endpoints }` with one row in `key::TOOLS`:

| id | host | auth | endpoints |
| --- | --- | --- | --- |
| `linear` | `mcp.linear.app` | `Bearer` | `/mcp`, `/mcp/readonly` |

The table is small and explicit for `key::PROVIDERS`' reason: `key set` refuses an id no row names, because a stored
key for a host nothing serves is a key nothing can spend. `Store::set_tool` / `Store::remove_tool` /
`Store::tools` / `Store::tool_key` mirror the provider methods, through the same temp-file-and-rename write, so the
file is never observable half-written and its mode is 0600 before the rename.

One file rather than a second secrets file: it is one mode to enforce, one directory `hs_key` mounts, one
`ensure_file` to call at boot, and one thing `server deploy` stages. A second file would need every one of those
twice, and the two would drift in exactly the way that leaves a secret group-readable.

## D2 — a tool key is not a provider row

Four facts hold, and each is pinned by a test:

- `linear` is not in `key::PROVIDERS`, and `key::provider("linear")` is `None`.
- It is never `default_provider()`: a machine's default is a model provider, and the refusal a machine with no key
  reads keeps naming that one.
- `key::models()` names no tool. A tool has no row in pi's bundled catalog — no context window, no cost, no
  reasoning levels — and a `/linear` base URL matches no `PROVIDERS` row, so the key proxy refuses it 403
  (`UnknownProvider`) before any forward: a `models.json` row for a tool would be a row pi could not resolve even if
  it had catalog metadata.
- `key::contains_real_key` covers both tables. The promise is about a real key in a session mount, and a tool key is
  a real key; a session directory carrying one is the same failure a provider key would be.

The CLI says so in both directions: a provider id given to `key revoke` is refused naming `key remove`, and a tool id
given to `key remove` is refused naming `key revoke`. `key set` takes either and stores it in the table that names it.
`key list` prints a provider id alone and a tool id as `<id> (tool)`, so the line a script parses keeps its shape and
the operator can still see which kind of credential they are looking at.

## D3 — the per-request read is the rotation mechanism

`Store::tool_key(id)` reads `keys.toml` from disk on **every** call; nothing caches it. That is not an optimisation
missed, it is the design: `key set linear` on a running machine is a rewrite of the file, and a caller that asks per
request spends the new key on its next request with no restart, no unit reload and no other file touched. The
alternative — the key read once at start — would make a rotation a service restart, which is the failure a key store
exists to avoid.

`key revoke` is idempotent for the same reason a rotation is a sequence: a second revoke is the state the operator
asked for, so it prints one sentence and exits 0. `key remove` keeps its exit 1 on a provider with no key, because a
rotation that did not happen should not read as one that did.

## D4 — a `[providers]` row naming a tool is read as a tool key, and moved

The machine this was written for already has `linear` under `[providers]` — a row an operator added by hand before the
tool table existed, because `key set linear` was refused and the row was the only way to keep the key; today's `parse`
accepts the row and `models()` silently skips it. So `parse` reads a `[providers]` row whose id names a `TOOLS` row as
a **tool** key and not as a provider, and `Store::normalize` rewrites such a file into the shape this build renders —
called from `ensure_file`, which every boot that starts the key proxy and every `key set` already goes through. It is
best effort there: a file this step cannot rewrite still *reads* correctly, so the boot that mounts it is not the
place to refuse it, and the proxy reports a file it cannot parse per request as it always has.

The move is deliberately narrow: only a row whose id names a tool moves. A row naming neither a provider nor a tool
stays exactly where it is — that is a stale id, not a mislaid table, and it must not start failing. A file with no
such row is left **byte for byte** alone, whitespace and table order included: this is a migration, not a formatter,
and `server deploy`'s kept file has to survive a boot unchanged. A file naming the same id in both tables is one this
build never writes; `[tools]` wins, because it is the table the key belongs in.

## Rollback

The store's parser is a **closed list** — `providers`, `tools`, `sessions` — and so is the release before this one's:
that release refuses a file carrying `[tools]` ("the key file's line 1 names an unknown table [tools]"), and the key
proxy answers **503 for every model request** when the store will not load. A rollback that left a `[tools]` table
behind would therefore look healthy — the upgrade's health poll asks `/api/health`, never a model — while every
session's model call failed.

So the table is written **only when a tool key is stored**. A machine that never stored one keeps a file that is byte
for byte what the previous release writes, and a rollback across this change is uneventful there. A machine that did
store one can make its file readable again with `key revoke <id>` (or by deleting the `[tools]` table) before
switching `current` back; nothing else about the file needs restoring, the migration in D4 being the only change to
its rows.

## What was refused

- **A tool key as a provider row under a second heading.** `key set linear` would then cost the four facts in D2, and
  `models()` would have to carry a "this one is not a model" test it has no business having.
- **An arbitrary id under `[tools]`.** Accepting any string would let an operator save a key for a host no row names,
  which is a key that sits in a 0600 file and is spent by nothing.
- **A config key, a schema migration, or a Docker mount.** None of the three is needed: the file is already mounted as
  a directory, it is not a database table, and the tables are the file's own.
- **The tool proxy in this change.** The key has a home; the thing that spends it — a proxy that serves a `TOOLS`
  row's host and swaps the real key in — is a separate change, and this one does not touch `keyproxy.rs` or the
  container paths to invent a route for it.

## What pins it, and what is not measured yet

`key::tests::the_tools_table_names_one_host_and_no_tool_is_a_provider` pins the row, the four D2 facts and a
`models.json` that names neither the tool nor its key; `a_tool_key_round_trips_replaces_itself_and_renders_under_tools`
the round trip, one row after two sets, `remove_tool` true then false, and the file's rendered shape;
`a_legacy_provider_row_naming_a_tool_is_read_and_persisted_as_a_tool_key` the migration, the stale row left alone and
the 0600 mode of the rewrite; `the_tools_table_wins_when_a_file_names_one_id_in_both` that a file naming one id twice
reads `[tools]` and ends up with one row, whichever order the tables are in; `normalize_leaves_a_file_without_a_legacy_row_alone`
the byte-for-byte promise; and `the_empty_store_is_the_rendering_of_the_empty_secrets` the one spelling `server deploy`
stages — `[providers]\n[sessions]\n` with no `[tools]`, and the table present as soon as one key is stored.

`tests/key_cli.rs` drives the real binary for the halves an operator sees: `key set linear` from a pipe stores under
`[tools]` in a 0600 file inside a 0700 directory and never prints the value, `key list` shows `linear (tool)`, two
sets leave one value, `key revoke linear` takes the value with it and a second revoke exits 0, each command refuses
the other table's id naming the command that works, and `linear` never becomes a provider id or a `models.json` row.
`a_tool_key_rotation_is_seen_by_the_next_read_of_the_file` runs `key set linear` twice — two writes from **another
process**, which is the shape a running machine's rotation has — and reads the file twice through `Store::tool_key`,
which is the reader a forwarded request uses, with the secrets directory asserted to hold one file.

**Not measured here:** a forwarded request carrying a rotated tool key. That proof needs the proxy that serves a
`TOOLS` host, which does not exist in this tree — the key proxy routes model providers only — so the wire-level
rotation belongs to that change, the way ADR-0050 left the provider half unmeasured until a real key was on the
machine.
