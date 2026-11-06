# Building Holosmith

The only artifacts are static musl binaries for `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl` (ADR-0005).

```bash
. ~/.pi/toolchains/env.sh     # rustc + both musl cross toolchains + qemu on PATH
scripts/build.sh              # web app, both targets, then scripts/check-static.sh
```

`scripts/build.sh` builds the web app first (`scripts/build-web.sh`, ADR-0007) and then
each target, copies each artifact to `dist/<target-triple>/holosmith` (by rename, so an
interrupted build never leaves a truncated file there), prints its SHA-256 and size, and
finishes by asserting that both are plain static binaries that run with no loader and no
userspace dependencies. `--target <triple>` builds one, `--no-check` skips the assertions.
`scripts/check-static.sh` can be re-run on its own; it also refuses an artifact whose
`git_tag` is not the tag of this tree, so a stale `dist/` cannot pass.

## Gates

The gate set for this repository is:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`Cargo.toml` sets `unwrap_used` to `warn`, and a warning cannot fail a build, so
`-D warnings` is what turns it into a gate. `scripts/build.sh` and
`scripts/check-static.sh` are the artifact gate, and `scripts/build.sh` runs
`check-static.sh` itself, so `scripts/verify.sh` calls `build.sh` once instead of checking
the artifact a second way. `scripts/verify.sh` also calls `scripts/check-embed.sh`, the
HS-003 proof of the three embed cases (app present, absent, required): it runs a nested
`cargo build` and so cannot be a `cargo test`, and it is cheap enough to call from the gate.

`scripts/verify.sh` is one command that runs all of it: the three commands above, the
artifact build and its checks, and the boot described under **One-command verification** below.

`cargo test` needs no node and no `web/dist`: without them `build.rs` embeds a placeholder
page that says the web app was not built, and the asset test in
`crates/holosmith/tests/http_contract.rs` prints a `SKIP` line instead of asserting the
built app's files. A green `cargo test` therefore proves the code, never that the app was
built — `scripts/check-embed.sh` and `scripts/build.sh` are what prove that.

The empty-root run proves there is no dynamic loader and no userspace dependency. It is
not a container run: HS-001's "both run in an empty container" clause stays open until
HS-046's verification run on real Docker hosts (ADR-0005).

`scripts/check-static.sh` carries the assertions HS-003 and HS-004 added, about what the
artifact ships rather than about the ELF: the artifact does not contain the placeholder
marker `build.rs` writes when `web/dist` is missing (so a build without the app cannot
ship), the app it serves from the empty root is byte-for-byte this tree's
`web/dist/index.html`, the migration SQL is compiled in (a byte grep of the artifact for
`CREATE TABLE schema_version`, which survives `strip` and LTO), and under the `--data-dir`
it was given it writes `app.db` and its WAL sidecars — and nothing else, before or after the
stop. The marker is read out of
`crates/holosmith/build_support/web_embed.rs` rather than restated here or in the script.

## One-command verification

```bash
. ~/.pi/toolchains/env.sh
scripts/verify.sh
```

Seven phases, cheapest first, so a formatting slip cannot cost a two-minute musl build:

| Phase | Command |
|---|---|
| `fmt` | `cargo fmt --check` |
| `clippy` | `cargo clippy --all-targets -- -D warnings` |
| `test` | `cargo test` |
| `migrations` | `cargo test --test schema_previous` (the expand/contract rule, HS-075) |
| `build` | `scripts/build.sh` (which runs `build-web.sh` first and `check-static.sh` last) |
| `embed` | `scripts/check-embed.sh` |
| `serve` | the boot below and its cases |

`cargo test` proves the code and never that the app was built, so the artifact phases come
after it and before the boot: the binary is built (phase 5) before it is served (phase 7).
`migrations` is its own phase for a reason a test list cannot give: the rule it checks is one a
migration author has to know about, and a failing run should name it (ADR-0047).
`scripts/build.sh` is called once, because it already runs `scripts/build-web.sh`; calling
both would run `npm ci` twice.

`serve` boots the shipped artifact, `dist/<host-triple>/holosmith` with the triple taken from
`uname -m` (`x86_64` and `aarch64` as ADR-0005 names them), not `target/debug`: this is the
thing that ships, it needs no loader, and it is the artifact `scripts/check-static.sh` has
already proved. A debug binary would embed whatever `web/dist` happened to exist when cargo
last ran, which is a different question. An artifact that is not there fails with a message
naming the path and `scripts/build.sh`.

The boot is `serve --config <work>/config.toml --port 0 --data-dir <work>/data`. `--port 0`
asks the kernel for the port and is also what makes `serve` re-derive `control_origin` from
the bound address (ADR-0007), so the case for a request that passes the guards sends exactly
the origin the guard compares; `control_listen`, `control_origin` and — since HS-041 — the
**preview listener's** bound address are all read from the startup banner, never assumed, because
the config asks the kernel for that port too (`preview_listen = "127.0.0.1:0"`) so that a boot of a
case's own and this one cannot collide over `8081`. A boot whose preview could not bind is a `warn`
and the banner says so, so `start_serve` fails rather than pretending: the case that exercises the
listener needs it. The data root is inside the work dir, so a run never touches
`/var/lib/holosmith` (ADR-0009), and the config file carries `owner`, `max_live_sessions = 1`,
`backend = "stub"`, `idle_stop_min = 1`, `turn_timeout_min = 1` and `preview_listen`: the
`bad_owner` case, the create cases, provisioning, the idle stop, the turn timeout's quiet half and
the preview case are the only reasons that file exists. Some cases bring a boot of their own, because the setting they need must not be process-wide.
`case_token_budget` stops the server and starts the same artifact with `config-budget.toml` and
`data-budget/`, because `max_session_tokens` is process-wide and every other case prompts a session of
its own — with a ceiling under the stub's per-turn spend, a dozen assertions would be reading the budget
instead of what they are named for. That
boot's banner and log live in `serve-data-budget.out` / `.err`, which `start_serve` names after its
data dir precisely so a case cannot read the other boot's. The backend comes from the *file* rather than from a `--backend` flag on purpose — that is half
of HS-011's "selectable by config and by `--backend`", and `case_backend_from_config` is what proves it;
`tests/serve_cli.rs` covers the flag, the override and the default. Every request is a `/dev/tcp` fetch (`fetch` for a body-less request,
`fetch_body` when a body is needed, `fetch_stream` for a response that never ends), so the
harness needs neither `curl` nor `jq`. `fetch_stream` exists because `fetch` reads until EOF:
an event stream sends its header block at once and then nothing until the first event, so
`fetch` would sit in its ten-second deadline and report a working stream as a broken one.
`fetch_stream` reads the header block line by line under bash's own three-second timeout and
stops at the blank line, so an early close, a `404` or a `text/plain` refusal is still saved
and asserted like any other answer. `fetch_sse_until` goes one step further and keeps reading body
lines until one contains a needle — an event stream never ends, so "read until the event that
settles the question" is the only bounded way to watch a producer.

A `serve_cases` array names the `case_*` functions, one per answer a browser gets: the health
payload, the schema version the banner reports against `version --json`, the app shell, the page
that holds the session list and the new-session form, byte-for-byte against
`web/dist/index.html`, the `404` envelope of ADR-0008, one case per guard refusal (`bad_host`,
`bad_origin`, `bad_client`, `bad_owner`), the request that passes all four, the exempt health
probe, the create endpoint (`case_create_session`, `case_create_is_idempotent`,
`case_create_bad_project`), the sessions list (`case_sessions_list`,
which asserts `200`, `application/json`, the session `case_create_session` made and a
`network_default` field the create form's checkbox starts from), the session's event stream
(`case_events_stream`, which reads the stream's header block with `fetch_stream` and asserts
`200`, `text/event-stream` and `no-cache` on the session `case_create_session` made), the backend
the boot chose (`case_backend_from_config`), the provisioning that create starts
(`case_provisioning_steps`, which reads that session's stream until the event says `idle` and
asserts the four step words arrived in order), and the state machine's payload
(`case_state_transition_payload`, which asserts the four step reports claim no state change and
the fifth is exactly `{"from":"starting","to":"idle","step":null,"reason":null}`), one
session by id (`case_session_detail`, which asserts the row the create made, that it carries no
`events` key, and that an unknown id is the `not_found` envelope), the session screen's path
(`case_session_screen`, which asserts `/sessions/<id>` answers the shell byte-for-byte), one tool
call's full output (`case_tool_output`, which plants a two-megabyte file under the session's own
`tool/` directory, asserts the route answers it byte for byte as `text/plain` off the artifact's data
root, and asserts that each spelling of a traversal is the `not_found` envelope — three levels up,
where the database is, so the check can fail; what the pump *stores* is `tests/rpc.rs`'s and
`tests/tool_output.rs`'s, because the stub's turn calls no tool), and the
prompt (`case_prompt_turn`, which asserts the 202 answers the row `running` and then reads that
session's stream until the turn settles, asserting the turn's six kinds in order and the
`idle → running → idle` transitions; `case_prompt_bad_body` and `case_prompt_unknown_session`,
which assert the `422 bad_body` and `404 not_found` envelopes), the spend across a stop and a resume
(`case_usage_survives_resume`, which prompts a session on a boot of its own, reads the row's
`usage_in`/`usage_out` out of `app.db` with `python3` (the API does not carry them — `Session` is pinned to its
fifteen keys), then evicts that session by creating a second one at a cap of one, resumes it, and asserts the
two counts are unchanged; without `python3` it prints one `SKIP` line), and the turn timeout's quiet half
(`case_turn_timeout`, which prompts the same session twice with `turn_timeout_min = 1` and asserts each
row comes back `idle` with no `status_reason` and that the runner wrote no deadline line — the gate
cannot make the deadline *fire*, because the key is whole minutes and the stub's turn lasts a second, so
that half is `tests/turn_timeout.rs`'s), and the abort
(`case_abort`, which prompts, aborts, and reads that session's stream until the event carries the
abort's own reason, asserting the 202 answered the row still `running` and that the turn ended
`running → idle` with reason `abort_completed`; `case_abort_on_an_idle_session`, which asserts the
`409 session_idle` envelope), and the steer (`case_steer`, which prompts, steers, asserts the 202
answered the row still `running`, reads the stream for the `turn.steer` frame carrying the guidance,
and then polls the row until it says `idle`, which is what proves the turn continued;
`case_steer_on_an_idle_session`, which asserts the `409 session_idle` envelope), the resume's two
refusals (`case_resume_on_an_idle_session` and `case_resume_unknown_session`; the round trip itself is
`tests/resume.rs`'s, because the session these two cases name is `idle` — HS-020's eviction leaves the
only `stopped` row of this run, and it is created last), and the delete (`case_delete_session`, which asserts the
202 answered the row `deleting`, then polls the row until it is the `404 not_found` envelope and the
list no longer holds it — the removal is a background task's, so the row is the proof;
`case_delete_unknown_session` and `case_delete_without_the_client_header`, which assert the `404
not_found` and `403 bad_client` envelopes, the latter because this is the crate's first `DELETE`
route and the guards must be shown to apply to it as they do to a `POST`), the idle stop
(`case_idle_stop`, which leaves a session alone for the config's one-minute `idle_stop_min` and
asserts the row is `stopped` with reason `idle_stop`, the transition in its replayed transcript, the
sweeper's one log line in `serve.err`, and that the next create at the cap is accepted without
evicting it — the slot the sweep freed; it is the only case that costs a real minute, because the
key is whole minutes and there is no shorter lever, ADR-0025), and the cap
(`case_create_at_cap_evicts`, which makes its own pair, creates at the cap, and asserts the victim's
row, its replayed transcript and the eviction's own `idle → stopped` event; and
`case_create_at_cap_refused`, which makes a session and asks for one more while that
session is still `starting` — live and not evictable — and asserts the `409 cap_reached` envelope and
the sentence that says nothing was idle), and exit classification (`case_exit_classification`, which
evicts a session of its own and reads the victim's `expected_stop` out of `app.db` with `python3`'s
`sqlite3` — no client is sent the flag, ADR-0038 — then resumes that session and asserts the new
container cleared it; the container's *exit* is `case_agent_launch`'s other half, below), the prompt's
turn coalesced (`case_event_coalescing`, which
makes its own session, prompts it, and asserts the replay holds no more delta rows than the script's
frames, one message id and the script's first and last sentences in order — the merge itself cannot be
shown here, because the stub paces eight commit windows apart; see ADR-0031), and compaction
(`case_compaction`, which boots its own server, prompts a turn, stops the artifact, pushes that session's
whole log 25 hours into the past with `python3`'s `sqlite3`, starts it again, and asserts one
`message.snapshot` row and no `message.delta` row, and a replay that carries the snapshot holding the
whole message, no delta frame, and the message's `end` frame still there — the window is the spec's fixed
24 hours rather than a key, so aging a database the artifact wrote is the only way to show it; see
ADR-0032), and history (`case_history`, which boots its own server with `history_window = 1`, drives
two turns, and asserts the `history.truncated` frame's cursor and turn count, that the replay's first
`id:` is that cursor, that the page below it carries the first turn with `has_more` false and
`turns_before` 0, and that a `before=0` answers `422 bad_query` with the envelope's content type — the
window is a key, so the shared boot must keep the default one; see ADR-0033), the project backup
(`case_backup`, which boots its own server with `project_backup = true` over a real repository, plants
eight day-named bundles, drives two creates, and asserts seven kept with the two oldest gone, exactly one
bundle for today, `git bundle verify` accepting it and it carrying a branch — the setting is a key, so the
shared boot must keep it off; see ADR-0034), and the worktree (`case_worktree`, which boots its own
server — `backend = "docker"`, a real repository under `projects_root`, and a socket that answers the
preflight's `_ping` — creates a session, and asserts the row's `worktree` and `base_commit`, the network
step's refusal, the worktree and branch gone afterwards, the registration back to one entry, and then a
`DELETE ?force=true` — a 202 and then a row that answers `404` — over the worktree that is already
absent), and the project's refusals (`case_worktree_refusal`, which boots a Docker run of its own over
a projects root holding a `.git` file, a repository with `.gitmodules` and one with
`extensions.worktreeConfig`, and asserts each create's `422` with its own code, `application/json`, no
session row afterwards and no `.worktrees` directory), and the hardened mounts (`case_container_mounts`,
which needs no server at all: it builds a real repository and worktree under `target/`, runs one real
container with the spec's four mounts and `--cap-drop ALL`, and asserts the three writes fail on the host
as well as in the container, the three git commands work, the process is not root and neither `mount
-o remount,rw /` nor `umount /work` is allowed — V1, see ADR-0035), the agent (`case_agent_launch`, which
boots a Docker run over a real repository **and a data root under `target/`** — the agent's directory is a
mount source, so the daemon has to see the data root too, ADR-0039 — creates a session, waits for `idle`
(pi answered a `get_state`), and asserts the seeded `<data>/sessions/<id>/pi-home/settings.json`, the launch
line in `session.log`, `docker inspect`'s command and environment, and that the row's `worktree` is
recorded; then it removes the container out from under the server and asserts the row becomes `error`
with a reason — spec item 6's third row, observed by the attached stdio HS-030 had no observer for — and
last that a `DELETE ?force=true` takes the row, the log, the worktree and the session's own directory;
see ADR-0039), and the resume over a live agent (`case_docker_resume`, which creates a session, waits for
`idle`, removes its container, waits for `error`, `POST /resume`s, and asserts the row reaches `idle`
again with a running container over the same worktree, then deletes its own session so the run's cap is
left as it found it; see ADR-0037/ADR-0039), and the turn (`case_docker_turn`, which prompts a session whose
container is pi in RPC mode and asserts the prompt is **accepted** by the handler and then ended by pi's own
answer — a session's network is `--internal`, its pi-home has no provider key (the gate's machine stores none,
so `models.json` has an empty provider object), so pi refuses the `prompt` command before a stream exists; what
the row carries is pi's sentence, not this build's; the
container must still be running afterwards, and a resume-then-prompt must work a second time, which is what
proves the RPC path is repeatable rather than a one-off and what found the removal-supersedes-the-launch bug
ADR-0040 records; see ADR-0040), and the key store and key proxy's refusal path (`case_key_proxy`, which needs no server, no daemon
and no internet: it writes a key with `key set` through stdin, asserts `keys.toml` is 0600 and `key list` prints
the provider and never the value, starts the shipped artifact as `proxy key` on a loopback port, and asserts a
request with no dummy key is `401` and an unknown provider is `403` before any provider is contacted; the
**real** forward is HS-040's recorded demo, because a gate that needs a real provider key is not a gate, while
the **bytes** the proxy hands its TLS client are `case_key_proxy_request`'s — a stand-in `openssl` on `PATH`
captures them, so one `Content-Length`, the body after the blank line and the swapped-in key are all gate facts;
see ADR-0048 and ADR-0050), the
WIP snapshot (`case_snapshot`, which needs no daemon: it builds a real repository and linked worktree under
`target/`, runs the artifact's `container snapshot` with `HS_SESSION_ID`, and asserts the ref has the changed
and new files while HEAD, the index and the working tree are untouched, then that a tree equal to HEAD removes
the ref; see ADR-0049), and the egress proxy's refusals (`case_egress_proxy`, which needs no server, no daemon and no internet: it starts the shipped
artifact as `proxy egress` on a loopback port with two allow-listed names and asks it four questions over a
plain socket — `GET` gets `405`, `CONNECT` to port 80 gets `403`, an unlisted host gets `403`, and
`localhost`, **on** the list, gets `403` because its address is inside the machine — then asserts the log's
one line per answer carries the session id from the proxy username, the host and the byte counts. The allow
path needs a real allow-listed host, so it is a unit test over a local echo server (`src/egress.rs`) and
the story's demo; a case that claimed to prove it would be proving nothing, or would need the internet
(ADR-0045); and the Internet-access checkbox (`case_internet_toggle`, which needs the daemon **and** the image and
starts the artifact on a `backend = "docker"` **config file** — the backend comes from the file rather than from a
`--backend` flag, as above — then creates two real sessions that differ only in the body's `network`. Two questions
are asked from a probe container on each session's own network — the internet directly, which must be unreachable
in **both** cases (the checkbox opens a proxy, not the route), and the `egress-proxy` alias, which must not resolve
in the off case and must answer `403` for a host that is not on the allow-list in the on one — and a third is read
from the two containers with `docker inspect`: every environment variable is identical once each session's own id
is normalised, which is the acceptance's "topology, not configuration". The image has no `curl`, so the questions
are node's `net.connect`, the same script shape the self-test's probe uses. The last assertion is the one no
container shows: the proxy is detached, the container is killed (the row's `unexpected_exit` lands it in `error`),
`POST /resume` brings it back, and the network's **endpoint count** is 1 for the off session and 2 for the on one —
detaching first is what makes that a rejoin rather than an inheritance, and a wrong flag cannot survive it; see
ADR-0046); and — since HS-041 — the **preview listener** (`case_preview`, which is two halves. The first needs
no daemon: on its own boot it asks the preview port for `/api/health`, `/api/sessions` and `/`, which must answer
`404` with a sentence rather than ADR-0008's envelope, for another machine's host name and for a well-formed id no
row holds, which must answer `404`, for an upgrade carrying a foreign `Origin` and one carrying none, which must
answer `403`, and it greps the shipped page's own script asset for the bytes the session screen's link is made of.
The second half is V7: it starts a container on an `--internal` `agents_<id>` named `s_<id>` the way the convention
names a session's container, asks the **host** directly first — so a failure says which half is wrong — then fetches
`/local/<id>/` through the shipped artifact, and does a WebSocket round trip with a hand-written handshake in
`python3` against a hand-written echo in the container's `node` (this build has no WebSocket crate and the image has
no `ws` package, which is exactly why the listener is a byte tunnel; ADR-0051). It needs the daemon **and** the
image and prints one `SKIP` line without them — the bug it exists to catch fails the first half, so a skip for "no
daemon" is not that bug. It boots a `stub` backend of its own, because a create on a Docker boot refuses a project
the machine does not hold and which boot is running when it starts is not its business).

One rule for every case above and below: a case that must assert one code keeps every higher guard check correct,
or it asserts another guard's code under its own name.

The CORS sweep is deliberately **not** in that array: it runs once after every case and checks
that no answer the run saved carries an `access-control-*` header or `vary: origin`. Being
outside the loop is what makes it cover a case a later story appends, without anyone having to
remember to place the new case before it.

**Every later story adds at least one `case_*` function and one line in `serve_cases`.** That
is what makes this a growing gate rather than a snapshot: a story that adds a route or a
behaviour has one obvious place to prove the shipped binary does it. A case that needs a setting the
rest of the run must not carry gets a boot of its own (see `case_token_budget`, `case_usage_survives_resume`,
`case_compaction`, `case_history`, `case_backup`, `case_worktree` and `case_worktree_refusal`), the
Internet-access checkbox (`case_internet_toggle`, described above), and a database from a newer release
(`case_schema_newer`, which needs no daemon: it boots the artifact twice over one data root, rewrites the
recorded `schema_version` to 99 in between with `python3`'s `sqlite3`, and asserts that the second boot's
banner prints `schema 99`, that its one `warn` names both versions and the command that will refuse
(`doctor`, HS-045), and that `GET /api/health` still answers `200` — the "serve continues so a rollback
needs no restore" half of ADR-0047), and takes a place at the end of the array: nothing after it may
assume the first boot's state, and a case that stops the server leaves the run with whatever boot it
started.

**The idle stop runs for the whole `serve` phase.** `idle_stop_min = 1` is the process's setting, so
every case from `start_serve` on has a sweeper behind it and **no case may leave a session `idle` for a
minute** — a later case that did would see its own subject turn `stopped` under it, and the failure
would blame the wrong case (ADR-0025). `case_idle_stop` is the one case that waits that minute, and it
runs where the table is otherwise empty.

`--no-build` skips `build` and `embed`, for a loop on the cases against an artifact already
in `dist/`; it does not skip `fmt`, `clippy`, `test` or `serve`. `--keep` keeps the work dir
of a successful run, and `-h` prints the script's own header comment.

The failure contract: a failure prints `FAIL <assertion>` (`FAIL build/failed`,
`FAIL case_guard_bad_origin/code`) and keeps the work dir, printing `work dir kept: <path>`. For
a failure in the `serve` phase that dir holds `config.toml`, `serve.out`, `serve.err` and one
`.http` and `.body` file per answer; a failure in an earlier phase keeps an otherwise empty dir,
because nothing has been written into it yet. A successful run removes it unless `--keep`, and
ends with `verify: <n> assertions, ok`. Teardown sends `SIGTERM`, waits, and asserts the artifact
exited 0 (ADR-0007), which `scripts/check-static.sh` cannot do because `bwrap` reports the
signal's own status instead.

## Browser-facing guards

The control listener refuses a request that is not your browser before any route sees it
(ADR-0010). On every request that is not a health probe the `Host` header must be in
`allowed_hosts`; every non-`GET` request must additionally carry the control origin as its
`Origin`, `Content-Type: application/json`, and `X-Holosmith-Client: 1`; and when `owner` is
set, a request carrying `Tailscale-User-Login` must carry yours. Only `localhost` and
`127.0.0.1` are implicit. A machine reached by its Tailnet name must therefore list that name
in `allowed_hosts`, and a browser that reaches the control listener over IPv6 loopback must
list `[::1]`; both are refused as `bad_host` otherwise. A refusal is the JSON envelope of
ADR-0008 with the code `bad_host`, `bad_origin`, `bad_client` or `bad_owner`, and no response
ever carries a CORS header.

`GET /api/health` is exempt from all four checks — `is_guard_exempt` is the one list — so the
probe works from `systemd`, from `curl` and from the hub's host registration.

The guard assertions are `crates/holosmith/tests/guards.rs`, and `scripts/verify.sh` lifts
them into the one-command check: the `case_guard_*` functions boot the shipped artifact and
require `bad_host`, `bad_origin`, `bad_client` and `bad_owner` from the requests that must be
refused, `404 not_found` from the request that passes all four, `200` from the exempt health
probe, and no CORS header on any answer. They are still not in `scripts/check-static.sh`,
which proves what the artifact ships rather than how it answers. `tests/guards.rs` remains
the unit-level proof, and the place the cases one boot cannot reach live: repeated headers,
`Origin: null`, each `X-Holosmith-Client` value, and a handler that tries to add a CORS
header.

## The database and its migrations

`serve` creates the data root and opens `<data_root>/app.db` before the listener binds, so a
data root that cannot be written is a startup failure and not a half-served process
(ADR-0009). The schema is built from numbered SQL files in `crates/holosmith/migrations`,
embedded at compile time with `include_dir!` and applied in one transaction at startup; a
new migration needs no build-script edit beyond the `cargo:rerun-if-changed=migrations` line
`build.rs` already carries, which is what makes a new file reappear in the next build. The
artifact is the authority: `holosmith version` reports `schema_version`, the startup banner
prints `database <path> schema <n>`, and `scripts/check-static.sh` proves both the SQL and
`app.db` are in the shipped binary.

A fresh build of this crate is a fresh test of the embed: `cargo test` runs
`tests/db_migrations.rs` against temporary files, because WAL cannot be observed on an
in-memory database. `cargo test` also needs no writable `/var/lib/holosmith`: every `serve`
test names a data root inside its own temporary directory, because `serve` now creates and
migrates whatever root the config resolves to, and an omitted key resolves to the built-in
default.

**The event log is not append-only for ever.** Compaction (HS-060, ADR-0032) replaces the delta
rows of messages completed more than 24 hours ago with one `message.snapshot` each, so an `events`
row can disappear while `sessions.last_seq` stays where it was. Two consequences for anyone
writing a statement against either table: a seq is **strictly increasing with gaps**, and the next
seq is `last_seq + 1` rather than `MAX(seq) + 1` — deriving it from the rows would reuse a seq a
snapshot already holds. Compaction runs in the background on a one-minute tick, folds one session
per transaction, and never touches a session whose state is `starting`, `running` or `deleting`.

### The expand/contract rule, and the one file to refresh

A migration may **add**. It may not drop or rename anything the previous release still uses —
that is what makes `holosmith rollback` a symlink switch rather than a database restore (spec
item 10, ADR-0047). Two things enforce it, and they catch different mistakes:

- **A destructive statement in a file that is not named as a contract migration is a test
  failure, immediately.** `schema::contract_violations` refuses `DROP TABLE`, `ALTER TABLE …
  DROP COLUMN` and `RENAME` in any migration whose slug is not `contract` or `contract_*`,
  matched on words rather than substrings. So the cleanup that follows an expand ships as
  `0005_contract_drop_old_column.sql`, one release later, and a release that only expands cannot
  contain one by accident.
- **A snapshot of the previous release's schema is compared with this build's**, as a subset:
  every table and every column in `docs/schema/previous.sql` must still exist. Drop or rename one
  and the subset test fails with the two ways forward in its message. It is not under
  `crates/holosmith/migrations` on purpose — that directory is embedded and byte-grepped into the
  artifact by `scripts/check-static.sh` — and the comparison is done by SQLite (`sqlite_master`,
  `pragma_table_info`), not by a second SQL parser.

`scripts/verify.sh` runs the subset test as the `migrations` phase, so a broken rule is named in
the output rather than buried in a test list.

**Refreshing the snapshot is a release step.** Take the release you are leaving — the simplest is
to start its binary once with an empty `--data-dir`, which migrates it — and run:

```bash
scripts/schema-snapshot.sh <data root>/app.db    # writes docs/schema/previous.sql
```

Commit the result *with* the change that needed it. A contract migration changes this file in the
same commit, so the diff is the record of what that release gave up; that is the only mechanical
form the "one release later" clause can take, because nothing in a repository knows which binary
was promoted. The first file of a project's life is necessarily that build's own schema, since
there is no earlier release to dump; the file's provenance line says which release it came from,
so a later reader is never guessing.

A worked pair, as a migration author would write it:

```sql
-- 0004_expectation.sql — release N: expand. Add the new shape, keep the old one working.
ALTER TABLE sessions ADD COLUMN expected_stop INTEGER NOT NULL DEFAULT 0;
```

```sql
-- 0007_contract_drop_retry_count.sql — release N+3: contract. The code stopped reading `retry_count` in N, and the
-- snapshot (docs/schema/previous.sql) is refreshed here, in this commit, so a reviewer sees both halves.
ALTER TABLE sessions DROP COLUMN retry_count;
```

If you are tempted to write the second one in the same release as the first, the subset test is
what will stop you: `retry_count` is still in the snapshot, and the message says either to keep it
or to refresh the snapshot and say why rolling back past this release is acceptable.

**A newer database is not a refusal to start.** If `schema_version` is *higher* than the
binary's `schema::SCHEMA_VERSION`, `serve` warns and continues: an older binary over a newer
database is exactly the rollback expand/contract exists to make safe. `schema::verdict` holds that
decision and the sentence, the banner prints the **recorded** number (`database <path> schema
<n>`), and `doctor` (HS-045) is where the sentence becomes a refusal — a check that runs before a
start, rather than a service that refuses to come back up.

## Prerequisites

- **rustc 1.99.1** (the version in `~/.pi/toolchains`). There is no `rust-toolchain.toml`:
  the build fetches nothing beyond its declared dependencies (HS-001).
- **`x86_64-linux-musl-gcc` and `aarch64-linux-musl-gcc`** on `PATH`. SQLite is compiled
  from source (`rusqlite` with the `bundled` feature), so the C toolchain crosses too.
- **node and npm** for the app shell: `scripts/build.sh` runs `scripts/build-web.sh`
  first, which runs `npm ci` from `web/package-lock.json`, then `npm test` and then Vite 7's
  build. Vite 7 needs node 20.19 or later; `npm test` is node's own runner over the
  transcript's rules in `web/test/` (ADR-0018), and it imports TypeScript directly, so it
  needs native type stripping: **node 22.18 or later** (22.6 through 22.17 need
  `--experimental-strip-types`, and 23.6+ is fine). Without node and npm `scripts/build.sh`
  stops before cargo with a message naming this file. A bare `cargo build` never runs npm.
- **`openssl`** in the agent image for a Docker run whose key proxy is used: `holosmith proxy key` runs
  `openssl s_client` to terminate the session's plaintext request and speak TLS to the provider (the artifact is
  static and has no TLS stack, ADR-0048). The image this repository tests with carries it; a missing binary is a
  502 from the proxy, not a silent fallback.
- **git** for `scripts/build.sh`, and for a Docker run at runtime: the worktree step runs
  `git worktree`, `git rev-list` and `git branch` against the project repository on the host
  (HS-025). Not required by `cargo build` itself, and not required by `--backend stub`.
- Optional: **`python3`** for the parts of the gate that read a database or speak a socket by hand:
  `case_worktree` (answers the Docker preflight from a socket saying `200 OK`, so a Docker boot can be driven
  on a machine with no daemon), `case_compaction` (ages a session's event log with stdlib `sqlite3`, because
  the compaction window is the spec's fixed 24 hours rather than a config key — ADR-0032),
  `case_exit_classification` and `case_usage_survives_resume` (read a column the API does not carry), the
  history case, and `case_schema_newer` (writes a `schema_version` from the future). Without it each case
  prints one `SKIP` line and the run passes; `cargo test` covers the behaviour either way.
- Optional: **`python3`** for `scripts/schema-snapshot.sh`, the release step that refreshes
  `docs/schema/previous.sql`: the `sqlite3` CLI is not installed and nothing in the gate may depend on it.
  That script is not part of `verify.sh` — it is run by hand when a release is cut (ADR-0047).
- **The development sandbox binds the host's Docker socket**, because every story from HS-027 on is a fact
  about a real `docker run` (`bwrap.sh`'s `DOCKER` binds). That is host root by another name — anything able
  to reach the socket can start a container with `/` mounted — so it is granted deliberately and only when
  the socket exists; a machine without a daemon gets no binds and every Docker check skips. A reviewing
  agent inside this sandbox can therefore create containers, which is the point and also the risk.
- **The mapping's fixtures** are committed, not generated: `crates/holosmith/tests/fixtures/pi-turn-*.jsonl`
  are five real turns' records from the image the Docker stories run — one answer, one that thought first, one
  that called a tool, one whose tool result carried an image, and one in which a project-local extension asked
  the client a question — replayed by `tests/rpc.rs` through the production mapping (ADR-0040, ADR-0041,
  ADR-0043). They are refreshed by hand — one prompt against a container with the operator's own provider
  credentials and network access, in a scratch agent directory under `target/` because a mount source the
  daemon cannot see is an empty directory, not an error — and `tests/rpc.rs`'s
  `a_fixture_whose_records_drifted_fails_this_guard` is what makes a silent refresh visible (it counts the
  record types per fixture, which a mapping assertion cannot: a frame cannot be missing for a record that is
  not there).
  - The tool fixture's capture needs a prompt that makes the agent call a tool (``Use the bash tool to run
    exactly: …``).
  - The **image** fixture needs a tool result that carries an `image` block, which `read` on a PNG produces; the
    picture lives in the container's working directory, so the prompt names a path under it
    (``Use the read tool on /work/pixel.png and then say…``).
  - The **extension UI** fixture needs a project-local extension — `.pi/extensions/<name>.ts` in the working
    directory, calling `ctx.ui.select(...)` from a `turn_start` handler — and the run must be launched with
    `--approve`, which is what makes a project's extensions load. Its `notify` after the dialog is the record
    that proves the cancel arrived: its text is what the dialog answered.
  - **Every fixture's last line must end with a newline**: the reader splits on LF, so a final record without
    one waits for EOF — which a fake agent that keeps its stdout open never gives.
  - **The capture must keep pi's stdin open until `agent_settled`.** Closing it is pi's orderly shutdown
    (`docs/rpc.md`), so a capture driven by `docker run -i < prompt.txt` records the system prompt and stops:
    a small driver that writes the prompt, reads stdout line by line, and answers a dialog with
    `{"type": "extension_ui_response", "id": …, "cancelled": true}` is what produces a complete turn. Answering
    the dialog is also what the build does, so the fixture is the same exchange production code has.
  - No test that *replays* a fixture needs a provider key, a daemon or the network, because the records are
    already committed; only the gate's `case_docker_turn` needs the daemon and the image, and it skips without
    them like every other Docker case.
- **A tool's output is a file under the data root** (HS-033, ADR-0041): `<data>/sessions/<id>/tool/<tool_id>.txt`,
  capped at 5 MiB of output plus one `[truncated: …]` marker line, served by
  `GET /api/sessions/{id}/tool-output/{tool_id}`. Nothing has to configure it and no test has to clean it up
  beyond the data root it already uses: the directory is a session's own, beside `session.log` and `pi-home`.
- **An image in a tool result is a second file in the same directory** (HS-035, ADR-0043):
  `<data>/sessions/<id>/tool/<tool_id>-<block index>.<ext>`, where the extension comes from pi's `mimeType`
  (`png`, `jpg`, `gif`, `webp`) and a type this build cannot name is kept as `.bin` and served as
  `application/octet-stream`. It is served by `GET /api/sessions/{id}/tool-image/{tool_id}/{index}`, the index
  being the block's position in `result.content`; `tool.end`'s `images` names the pair, so nothing about the
  type comes from the request. The cap is 5 MiB of **decoded** bytes, and a body over it is refused from its
  encoded length before the decode allocates. The transcript draws one link per picture rather than the picture
  itself (ADR-0043).
- Optional: a **Docker daemon and one image** for `scripts/verify.sh`'s `case_container_mounts` and for
  `tests/container.rs`. Both check V1 — the hardened `.git` mounts — against a real container, and both
  print one `SKIP` line and pass without a daemon, because a machine without one is a supported machine
  (a hub). The image is `$HOLOSMITH_TEST_IMAGE`, default `pi-agent:local`; it needs `sh`, `git` **and
  `node`** — the network self-test's probe is a node script (HS-036, ADR-0044), and an image without node makes
  that test `Unknown` rather than failed, so a missing node is a `warn` at every boot instead of a red test. Its
  default user must not be root. A development machine can build one, or tag the image it has:
  `docker tag pi-agent:local agent-image:<version>`. That image also runs the **egress proxy** (HS-037,
  ADR-0045), and it needs nothing added for that: the container executes *this* binary, mounted read-only at
  its own host path, so the artifact (static musl) is the whole requirement — which means the **daemon has to
  be able to see the binary's path**, exactly as it has to see the data root for the agent directory
  (ADR-0039). A `serve` whose binary sits on a tmpfs the daemon does not share starts no proxy: one `warn`,
  and a session that allows internet access reaches nothing through it.
- Optional: **`qemu-aarch64-static`** (or point `HOLOSMITH_QEMU_AARCH64` at one) to run
  the aarch64 artifact on an x86_64 machine. It is a machine-local dev tool installed
  outside this repo; no script here downloads it. Without it the check prints one
  `SKIP` line and still passes.
- Optional: **`bwrap`** (bubblewrap) for the empty-root run. Also machine-local; without
  it the check prints one `SKIP` line and still passes.

## Running with Docker

`backend = "docker"` is the default (ADR-0001), and `serve` will not start one on a machine
that cannot run it: the preflight checks the daemon, git and the disk before the listener
binds, prints one sentence per failure that names the fix, and exits 1 (HS-024, ADR-0028).
`--backend stub` skips all of it — the stub needs no daemon, no container and no free space
beyond its own database, which is what the gate uses and what a phone-only demo can use — and
so does `role = "hub"`, which runs no sessions at all: item 10 §6's `hub deploy` says the startup
runs "without the Docker and git checks", and a hub without a daemon is the normal hub. The check
at startup is why `scripts/check-static.sh`'s empty-container run passes `--backend stub`: that
run is about the artifact's own linking, and its sandbox has no `git`, no `df` and no daemon.

**A session's files live at `<projects_root>/.worktrees/<id>`, on branch `agent/<id>`** (HS-025,
spec item 1). The projects root is the config key `projects_root` (default `/srv/holosmith/projects`),
and each project is a repository directly inside it: `<projects_root>/<project>` with a `.git` directory.
The **backend** makes the worktree with `git worktree add -b agent/<id> <projects_root>/.worktrees/<id>
HEAD` and the **provisioner** writes the path and that commit to the session's row; the teardown —
`git worktree remove --force`, `git branch -D agent/<id>`, `git worktree prune` — runs when the session
is deleted, and when a later provisioning step fails *and this provision made the worktree*: a worktree
the step only adopted (a resumed session's, with commits on it) is never taken back by a failure. **No fetch and no remote**: the session branch is created from a commit
the project repository already has. Two consequences worth knowing before you point this at a real
repository: a repository with **no commit** cannot be provisioned from (there is no `HEAD`), and a
project whose `.git` is a *file* (a submodule checkout, or a worktree of a repository) is HS-026's to
refuse — this build tries it and git says what it thinks in the session's reason.

**The container's mounts are spec item 1's four** (HS-027, ADR-0035): the worktree at `/work`, the
repository's `.git` at its own host path (so the worktree's `.git` file, which holds an absolute
`gitdir:`, still resolves), and `.git/hooks` and `.git/config` stacked on top of it **read-only** so that
a hook or a `core.fsmonitor` cannot be planted. Host paths are kept identical inside the container for
exactly that reason; `container::Mounts` is the one value that spells them, and V1 — the three refused
writes and the three working git commands — passed as specified on a real daemon, so the spec's narrower
fallback is not in use. The image is the config key `agent_image` (default `agent-image:<version>`), and
`--user` is deliberately not passed: the image's own user must be non-root, which the V1 checks assert
rather than a guessed uid that would hand the agent's files to somebody the host does not own.

**The isolation is measured at every boot** (HS-036, ADR-0044). A Docker run creates the machine's own
`egress` network (not internal — the proxies attach to it) and then runs one probe container from `agent_image`
on a temporary internal network, asking it to reach `1.1.1.1:443` and the default bridge's gateway. A
*connection that is refused* counts as reached, because a reset proves the host answered; `ENETUNREACH`, a
timeout and the like are the pass. Three outcomes, one refusal: `Passed` is a `debug` line, `Unknown` (no image,
a daemon that cannot create networks, a probe that did not finish) is one `warn` and the boot continues, and
`Failed` refuses the boot with a sentence naming what was reached and the targets it tried — the same shape and
place as the preflight's complaints. `role = "hub"` and `--backend stub` run neither, and a machine with no
daemon at all is a supported machine: `tests/selftest.rs` and `case_network_selftest` both print one `SKIP`
line and pass.

**One container per session, limited and on a network of its own** (HS-028, ADR-0036): a
`docker run -i --rm --init --name s_<id> --network agents_<id> --cap-drop ALL --security-opt
no-new-privileges --workdir /work --cpus … --memory …m --pids-limit …` with the five mounts and
`-e HS_SESSION_ID=<id>`. The network is `docker network create --internal agents_<id>`; `--internal` is the
isolation, and it is why the container can reach nothing but its own network. Three keys bound it, all
integers because a config value has no decimal and Docker reads `0` as *unlimited*, so all three refuse
zero:

| Key | Default | Flag |
| --- | --- | --- |
| `container_cpus` | 2 | `--cpus` |
| `container_memory_mb` | 4096 | `--memory <n>m` |
| `container_pids` | 512 | `--pids-limit` |

Since HS-031 the container **is the agent**: `run` is attached (`-i`, never `--detach`) and ends with
`pi --mode rpc --no-session --approve --model <row> --thinking <row>`, with `PI_CODING_AGENT_DIR` pointing
at `<data>/sessions/<id>/pi-home` (a fifth mount, at its own host path) and the four switches that keep pi
off the network: `NO_PROXY=key-proxy`, `PI_OFFLINE=1`, `PI_SKIP_VERSION_CHECK=1`, `PI_TELEMETRY=0`. The
server holds that process's stdio — its stdout is pi's JSONL, its stdin takes commands, and its exit code
and stderr are what `exit::Exits` classifies. "Ready" is a protocol fact: a `get_state` command and the
`response` record that repeats its `id`, under `READY_TIMEOUT` (30 s); the version assertion is
`docker run --rm --network none --entrypoint pi <image> --version` against `pins/pi.txt`, asked once per
process; and `agent::launch_line` writes one line into `session.log` before the process starts. See
ADR-0039.

Three things bite here:

- **A mount's source has to exist before the container step**, so the agent directory is made and seeded
  first — and it lives under the **data root**, so a Docker session's data root must be a path the daemon
  can see, exactly as the projects root already had to be (ADR-0035).
- **The container appears seconds after the step returns.** The step's success means the process was
  spawned, not that the container is up; anything that inspects one waits for it (`tests/*`'s
  `wait_running`), and the gate's case waits for the row to say `idle`, which is after pi answered.
- **A killed docker CLI does not stop its container.** The container outlives a crashed server, which is
  HS-044's reconcile; the next start's `rm -f` is what makes it boring in the meantime.

**Every start is `docker rm -f s_<id>` then `docker run`** (HS-029, ADR-0037), and the two are serialised
per session, because two starts interleaved produce "the container name is already in use" — which is one
start working as designed being read as the other one failing. `--rm` alone is not enough: it removes an
exited container a moment *after* `docker stop` has answered (measured), and it does nothing about a
container the process lost track of. `stop` is `docker stop --time 10`; the ten seconds are the spec's, and every
stop goes through `stop::Stopper`, which first records `sessions.expected_stop` — the flag that makes a
policy stop distinguishable from a crash (HS-030, ADR-0038).

Three things bite on a real machine:

- **`docker_host`**: unset means `unix:///var/run/docker.sock`, the rootful socket. A
  rootless daemon lives at `unix:///run/user/<$UID>/docker.sock`, and `tcp://host:port` and
  `ssh://` are the other spellings the config validator accepts; `ssh://` is reported as *not
  checked* rather than passed, because this build cannot reach into another machine's daemon yet. Put yourself in the `docker` group or
  the socket answers `permission denied` — the preflight's sentence names the group fix, not
  `systemctl`.
- **git 2.38 or newer**, because the merge check runs `git merge-tree --write-tree` (V2).
  macOS' `/usr/bin/git` is a shim for the Command Line Tools and reports 2.20 until they are
  installed; the preflight says which version it found.
- **A mount source must be a path the daemon can see, and `-v` does not say when it is not.** Docker
  resolves a bind mount's source in the *host's* mount namespace, so a path that exists only inside a
  sandbox — a `mktemp -d` under a tmpfs `/tmp`, a namespace's private mount — is not a path the daemon has.
  `docker run -v` does not fail on that: it **creates** the source as an empty root-owned directory and
  mounts that, so the agent gets an empty `/work` and a `.git` that is not the repository, with no error
  anywhere. `container::Mounts::check` is the answer (`case_container_mounts` and `tests/container.rs`
  therefore keep their scratch repositories under `target/`, which is a real host path wherever `cargo
  test` runs).
- **Docker Desktop's published ports are the VM's, not the container's.** On a dev machine
  the Desktop VM forwards a `-p` port to the host, so a port a container "publishes" is
  reachable from that machine and not from your phone or the hub — and the forwarder is a
  second hop that `docker inspect` does not describe. That is why the control listener is
  the host's own process, why the preview listener (HS-041) is on the host too, and why
  `docker_host` on a development machine is a socket rather than a TCP endpoint.

## Verifying the credential path by hand (V3, V4, V5)

These three checks need something no gate may require: **a real provider key**. So they are run by hand, their
results are written into `HolosmithSpec.md`'s vertical-slice table, and the gate keeps only the half that needs
no key (`case_key_proxy` sends a request through the proxy with a stand-in `openssl` on `PATH`, and asserts the
bytes the proxy produces: one `Content-Length`, the body after the blank line, and the stored key swapped in).

What a run needs:

- a Docker daemon and `agent_image` (the same image the Docker stories use), because V3 and V4 are facts about a
  container and its mounts;
- a **real key for a provider in `key::PROVIDERS`**. Which provider is the operator's; the run that produced the
  table's V3/V4/V5 cells used `opencode-go` (opencode.ai's Go gateway), whose anthropic-shaped models are
  addressed under `/inference/go/anthropic`, and the model `opencode-go/claude-haiku-5-5` at `thinking = "max"`
  because it streams thinking deltas at that level;
- a git repository under `projects_root`, and a data root under `target/` — a data root the daemon cannot see
  mounts an empty pi-home (ADR-0039);
- `min_free_gb` low enough for the machine, exactly as the gate's Docker cases set it to 1.

The run itself:

```bash
. ~/.pi/toolchains/env.sh
scripts/build.sh                       # the demo runs the shipped artifact, not a debug build
printf '%s\n' "$YOUR_KEY" | dist/x86_64-unknown-linux-musl/holosmith --config target/v3/config.toml \
  key set opencode-go                  # stdin only: never an argv
```

Then, with `serve` running on that config:

1. **V3** — create a session with `model = "opencode-go/claude-haiku-5-5"` and `thinking = "max"`, wait for it to
   reach `idle`, and prompt it twice: once with a prompt that makes it call a tool (``Use the bash tool to run
   exactly: echo holosmith-v3``) and once with a question that makes it think. Read the transcript back from
   `GET /api/sessions/<id>/events/history?before=<big>&limit=1000` and assert `message.delta` frames of both
   kinds, one `tool.start`/`tool.end` pair, and `usage` on `message.end`. `docker inspect s_<id>` must show no
   provider key in the environment, and the pi-home must carry the dummy.
2. **V4** — in the same container, `printf '{"type":"get_available_models","id":"m1"}\n' | docker exec -i
   s_<id> pi --mode rpc --no-session` answers the bundled catalog with `PI_OFFLINE=1` and only the dummy key.
3. **V5** — read `docker logs hs_key`. The line for a real turn names the session (`session=<id> … status=forwarded`),
   which is the `models.json` dummy in use. Then put a competing value where the precedence order says it wins
   (`<data>/sessions/<id>/pi-home/auth.json`, `{"<provider>":{"type":"api_key","key":"…"}}`) and run one turn;
   the proxy now logs `status=bad-key` with `session=-`. Remove it and the turn works again. The provider's
   environment variable is the weakest source: `docker exec -e OPENCODE_API_KEY=…` does *not* displace the
   `models.json` key. **That is why the seeded pi-home carries no `auth.json` and the container environment no
   provider key** — the fallback in the table's last column is a rule about what the build must keep out.

Two defects this procedure found, both fixed in HS-040 (ADR-0050), are worth checking first when a model call
fails: a `502 the provider closed the connection before answering` means the forwarded request declared a
`Content-Length` it did not carry, and `401 … session=- … status=bad-key` for a session that exists means the
proxy is reading a `keys.toml` from before the create — a file mount rather than the directory.

## Where the target settings live

`.cargo/config.toml` names the two linkers and sets
`rustflags = ["-C", "relocation-model=static"]` per target, which is what makes the
artifacts `ET_EXEC` static binaries instead of static PIE. **Never export `RUSTFLAGS`**:
cargo silently ignores `target.*.rustflags` when that variable is set, so the build would
ship a static-PIE binary with no error. `scripts/check-static.sh` asserts `statically
linked` and an empty `readelf -d` so the mistake fails loudly instead.

## Git tag

`build.rs` reports the tag in this order: `HOLOSMITH_GIT_TAG` if set, otherwise
`git describe --tags --always --dirty`, otherwise `unknown`. `scripts/build.sh` always
sets it, so an artifact never reports the tag of a stale checkout. Builder containers
have no `.git` in the build context, so image builds pass `HOLOSMITH_GIT_TAG` as a build
argument (phase 2); that is why the fallback makes the build succeed instead of failing.
A bare `cargo build` does not set it, and because `build.rs`'s `rerun-if-changed` lines
opt out of cargo's default rerun, a new tag on an unchanged tree leaves the old value in
place: only `scripts/build.sh` guarantees a fresh tag.

## The pinned pi version

It lives in `pins/pi.txt` and nowhere else; the build embeds it, `holosmith version`
prints it and `doctor` will compare the agent image against it. **Do not repeat the
number in this file or anywhere else** — a test (`tests/pin_single_source.rs`) fails if it
appears in a second tracked file.

## Workspace layout

The Cargo workspace root has one member, `crates/holosmith` (ADR-0004), so commands from
the repo root need the package flag: `cargo run -p holosmith -- version`. Build output
goes to `target/`; shipped artifacts to `dist/`; both are git-ignored.

## Reproducibility

Building on this machine is enough for development. Promotion builds run in a pinned
builder container (spec item 10), so `build.rs` and proc macros never run host-side; the
host has to run only the resulting static binary. Until that container exists (phase 2),
`scripts/build.sh` is the documented path and `scripts/check-static.sh` is the evidence
that the artifact is what it claims.

The fetch-nothing claim has a runnable proof, run once per target:

```bash
cargo fetch --locked
bwrap --dev-bind / / --unshare-net --die-with-parent \
  cargo build --release --locked --offline --target <triple>
```

`scripts/check-static.sh` rejects a `source = "git+…"` entry in `Cargo.lock` for the same
reason, so the dependency set cannot start needing the network without a gate failing.

That proof covers cargo only, and it is not enough on its own since HS-003: the binary
embeds the web app, so a cargo-only build of a clone without `web/dist` still succeeds and
serves a placeholder page. Run `scripts/build-web.sh` first (that is what `npm ci` reads
`web/package-lock.json` for, the same category as `cargo fetch --locked`), and build the
artifact with `HOLOSMITH_REQUIRE_WEB=1` set — which `scripts/build.sh` always does, and
which turns a missing app into a build failure instead of a warning. `scripts/build.sh` is
the documented release path for exactly that reason.
