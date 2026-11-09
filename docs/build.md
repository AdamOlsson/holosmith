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
route and the guards must be shown to apply to it as they do to a `POST`), what the delete keeps
(`case_delete_keeps`, which makes its own session, deletes it, and asserts the 202 carries `kept` as
three explicit nulls — a stub session has no repository, and `kept: null` is a different answer,
"the repository could not answer", which the page renders differently — then greps the shipped
page's own script asset for the affordance, the forced-delete button, the sentence that renders the
answer's command and the notice for a delete that kept nothing; the keep-rule itself is
`case_delete_keeps_the_work` and `tests/worktree.rs`), the idle stop
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
session row afterwards and no `.worktrees` directory), and the changes view (`case_changes`, which boots
its own Docker run over a real repository and the same `_ping`-only socket, re-makes the worktree the
failed network step took back, commits twice on `agent/<id>` and once on `main`, writes an uncommitted
file, and asserts every field of `GET /api/sessions/{id}/changes` — `branch`, `base_commit`, `head`,
`target`, `ahead` 2 and `behind` 1 against `git rev-list --count` run by the case, `dirty`, `mergeable`,
`files[]` with a file's line counts and `touches_ci` — then plants a conflict and asserts `mergeable:
false` with the path *and* that no working tree moved, then removes and re-creates-as-unregistered the
worktree for the two `409 no_worktree` arms; no daemon is used, which is the story's own "no container is
involved"; see ADR-0061), and the unified diff (`case_changes_diff`, which boots its own Docker run over a
real repository and the same `_ping`-only socket, re-makes the worktree, commits a text file, a
NUL-bearing binary and a `seq`-generated file whose patch is past 256 KB, and then asserts
`GET /api/sessions/{id}/changes/diff`: the file's body byte-for-byte against `git diff` run by the case, the
whole-branch answer naming its files and capped with a marker, the binary file as git's own summary with no
NUL byte in the answer, the large file at most 262144 bytes with a marker naming git's own byte count, and
`..%2Fetc%2Fpasswd`, `%2Fetc%2Fpasswd` and an unchanged path each a `404 not_found` that leaks nothing,
then the worktree gone for the summary's `409 no_worktree`; see ADR-0062), the Changes sheet
(`case_changes_screen`, which is one of the two cases a **UI** story adds and deliberately uses no browser: it boots its
own Docker run over a real repository and the `_ping`-only socket, re-makes the worktree, commits a `Jenkinsfile`,
and asserts the three things a browser would otherwise be the only witness of — `GET /sessions/<id>` is this
tree's `web/dist/index.html` byte for byte, the **served** bundle (the `<script src>` in that page, fetched from the
artifact and compared with `web/dist/assets/…` byte for byte) carries the sheet's nineteen id needles and the three
sentences the ADRs fixed (the CI warning, ADR-0065 D4's commit prompt, ADR-0064 D10's Resolve prompt), and
`GET …/changes` carries `"choices":{"targets":["main","master"],"remotes":["origin"]}` — both existing
configured targets in the config's order, and a configured remote no repository here has, because that list is the
config's and unfiltered — with `touches_ci: true`; the sheet's *behaviour* is `web/test/transcript.test.ts`'s,
because no gate may need a browser; see ADR-0067), and the merge (`case_merge`,
which boots its own Docker run over a real repository and the same `_ping`-only socket, re-makes the
worktree, and walks **one** repository through both of rule 4's update paths: `git checkout --detach` makes
`main` unchecked-out, so a `POST …/merge {"target":"main"}` moves the ref and *not* the checkout (the agent's
file never appears there), and `git checkout main` then makes the same merge move the working tree as well —
with `git rev-parse` run by the case on both sides. In between it asserts the refused requests
(`bad_target` twice and `bad_strategy` for `strategy:"rebase"`) as status **and** code with the target unmoved
across them, and it ends with `target_dirty` (an untracked file the branch adds, untouched by the refusal and
merged once it is out of the way) and `not_fast_forward` (a commit on the target, with both refs and the checkout
unchanged); the default `strategy` is asserted to be `ff-only` on a body that names none. (`unsupported` for
`delete_session:true` was one of these until HS-056 built it.) No daemon is used — "all git work for review and merge runs on the host", which is the story's
own sentence; see ADR-0063), the two landing strategies (`case_merge_strategies`, which boots its own Docker run
over its own projects root **and its own data root** — `<data>/merge` is what it measures — re-makes the worktree
with the step's own command, and walks one session's history through all three: a **`squash`** of three commits
is `200` with `strategy:"squash"`, `git rev-list --count <base>..main` is `1`, `main^` is the old target and
`main^{tree}` equals `agent/<id>^{tree}` (the tree, because one commit that lost a hunk is still one commit); two
more commits then a **`merge`** is `200` with `rev-list --parents -n 1 main` exactly `<new> <squashed> <tip>`; a
planted conflict on `f.txt` is `409 conflict` with `"conflicts":["f.txt"]`, the target unmoved, the checkout's
`status --porcelain` empty and the operator's file untouched; and `<data>/merge` is `0` entries at the end,
measured with `find … -mindepth 1 | wc -l` after asserting the directory exists; see ADR-0064), the merge's guards
(`case_merge_guards`, which boots its own Docker run over its own projects root **and its own data root** — the
refused merges are measured by `<data>/merge` not existing at all — and measures six arms of spec item 12's rule 1
and rule 5: an **untracked** file in the session's worktree is `409 dirty_worktree` with the target, the branch and
the file untouched and no `<data>/merge`, while the same file **ignored** by a committed `.gitignore` merges; a row
whose `status` column is set to `running` (with python3's `sqlite3`, because a gate may not run a turn against the
`_ping` shim) is `409 session_running`, and the same row back in the `error` its failed provision left merges — the negative
half an `!= Idle` guard would refuse; `{"strategy":"squash","delete_session":true}` answers `200` with rule 6's
three keys and the branch's **tree**, then the row is gone, the worktree is gone and `git branch --list 'agent/*'`
is empty; a second session's squash **without** the flag keeps the branch, a `delete_session` whose snapshot cannot
be taken (its `sessions` directory made read-only) still answers `200` and leaves the row, and the ordinary `DELETE`
afterwards answers `kept.branch` naming the branch it keeps; see ADR-0065), the publish (`case_publish`, which
boots its own Docker run over its own projects root **and its own data root**, re-makes the worktree the failed
network step took back, and publishes into a **bare repository under `$WORK`** — `git init --bare` plus a
`git remote add origin` pointing at it, so a real push happens with no network, no daemon and no credential, and
the remote is on a path no container's mount vector names, which is what makes a `200` evidence that the **host**
pushed: `{"remote":"origin","confirm":true}` is `200` with exactly three keys (`remote`, `branch`, `commit` —
no `touches_ci`, which is the changes view's) and the branch's tip as the case read it with `git rev-parse`, then
`git rev-parse refs/heads/agent/<id>` **in the bare repository** names that commit and the local branch has not
moved; `branch:"pr/one"` creates *that* ref at the same tip and no local branch; the four request refusals —
protected `main`, the configured target `trunk` (`[merge] targets = ["trunk"]` is what makes the second fact
reachable at all), an unlisted remote and `confirm:false` — are `422` with `protected_branch`,
`target_not_pushable`, `bad_remote` and `confirm_required` and the remote's `for-each-ref` output unchanged across
every one of them; a second commit published and the branch then rewound is `409 push_rejected` with the remote's
refs still unchanged, which is "never force-pushes" as a fact about a ref; a **second boot** with git's
`GIT_CONFIG_COUNT=1` / `GIT_CONFIG_KEY_0=remote.origin.pushurl` / `GIT_CONFIG_VALUE_0=<a second bare repository>`
in the server's environment publishes `branch:"env/one"` and the case asserts it landed in *that* repository and
not at the URL the project's remote names — the arm that makes "the host's own git, with the host's environment"
a measurement, since `.env_clear()` on the push's command would turn exactly that pair red while every other arm
stayed green; and a **third boot** with `[publish] remotes = []` answers `403 publish_disabled` for the row *and*
for an id the table does not hold, with the remote untouched — the disabled endpoint is the process's config, so
it is a boot and not a request; see ADR-0066), and the hardened mounts
(`case_container_mounts`,
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
the ref; see ADR-0049), what a delete keeps against a real repository (`case_delete_keeps_the_work`, which needs the daemon
**and** the image and prints one `SKIP` line without them: it creates a real session on its own
`backend = "docker"` boot, waits for `idle`, simulates the agent's work with one commit on
`agent/<id>` and one uncommitted file, deletes the session, and asserts the answer named both names
and the command, that the branch tip and the ref are still in the repository, that the worktree and
the session directory are gone, and that `git branch rescue refs/holosmith/wip/<id>` recovers the
change; the rule needs no daemon at all, so `tests/worktree.rs` proves it on a `TempDir` repository
too — see ADR-0052), the startup reconcile in two halves (`case_reconcile`, which needs neither a daemon nor an
image and kills the artifact with `kill -9` mid-turn — the 202 puts the row in `running` before the stub's first
200 ms frame, and a second session is killed mid-provision — then restarts it on the same data root and asserts
the row comes back `idle · interrupted`, the transition is in the committed log, no row is `running`, and
`sessions.last_seq` is the log's highest with the seqs unique and gap-free; it needs `python3` for the two
database reads, and the counter rule needs no daemon so `tests/reconcile.rs` proves it too. And
`case_reconcile_orphans`, which needs the daemon **and** the image and prints one `SKIP` line without them: it
plants a container, a network and a registered worktree that no row owns, kills -9 a live Docker session, and
asserts the restart removed the orphans, replaced the session's container under the same name with a new id,
and left the branch and the snapshot ref alone — the container half cannot be proven on a machine that has no
daemon, which is the same `SKIP` shape every Docker case here has, while `tests/reconcile.rs` proves the
containment rule daemon-free; see ADR-0053), and the egress proxy's refusals (`case_egress_proxy`, which needs no server, no daemon and no internet: it starts the shipped
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

One case is about the *record* rather than the binary: `case_verification_table` reads every `V<n>` row of
`HolosmithSpec.md`'s verification table and asserts that no V1–V10 row still says "not yet run", that each carries
a verdict, that a `fail` row also names the fallback taken, that each names an automation this repository really
has (a `case_*` in `serve_cases`, a `tests/*.rs`, a `scripts/*.sh`, or the by-hand section below), and that V11
still says it is deferred to phase 3 (HS-070). It needs no server, no daemon and no image. The eleven-row count
is what keeps the read honest in both directions: a broken read (a renamed heading, a table that moved) fails
instead of passing over nothing, and a twelfth row is a deliberate edit here rather than a row the case walks
past. It runs first in `serve_cases`, so a table that has rotted fails the serve phase before any Docker case
rather than after ten minutes of them (HS-046, ADR-0055).

…and — since HS-045 — **`doctor` itself**: `case_doctor_quick` runs the artifact's quick list three times over a
machine it builds (a `_ping` socket it answers, two ports the kernel gives it, a data root under `target/`): once
on a machine that passes, where it asserts the report's header, one line per check with its outcome, that the
`systemd` and `tailscale` lines each say what they measured (both are `unknown` on a machine with no systemd and
no `tailscaled` daemon, and both are a `pass` where they are there), the free-port sentences and a tally with no
failure; once with a `python3` listener **holding** the control port, where `ports` must fail with the sentence
that names the holder and the exit code must be 1 (`ExecStartPre`'s whole point); and once with `docker_host` at a
socket that cannot exist, where the `docker` line carries the sentence `serve` refuses with and every other check
still prints. It needs `python3` and nothing else, and prints one `SKIP` line without it (ADR-0054).
`case_doctor_full` runs it at item 10 §7's moment — its **own** Docker `serve`, so the ports are held by this
machine's server, both proxies exist and the self-test has a daemon — and asserts `network`, both `proxy` lines,
`image` against `pins/pi.txt`, the two `ports` lines as the serving pass, exit 0, and that the self-test's network
is gone afterwards; it needs the daemon **and** the image, and prints one `SKIP` line without them, which is not
the bug it exists to catch (a missing `network` line fails while the machine serves). It stops the shared boot and
leaves its own running, so it is last.

…and since HS-062, **the disk floor on the request paths**: `case_disk_floor` boots its own Docker run with a
`df` **stand-in** on `PATH` — a `/bin/sh` script that `cat`s a two-line answer this case rewrites
(`deploy_machine_shims`' shape), because the boot itself refuses below the floor, so a case about the request
paths cannot start on a small disk — plants a healthy answer, creates one session (the row reaches `error`, the
`_ping`-only socket failing its network step, which is a state the resume accepts), and then lowers the answer
**under the running server**. It asserts `POST /api/sessions` answers `507` with `code: disk_floor`, JSON, and a
sentence naming the free space, `min_free_gb` and the data root; that `POST /api/sessions/{id}/resume` answers the
same for the row that already exists and leaves it `error` rather than `starting`; that `GET /api/sessions` still
answers `200` with the row and never carries the floor's code; and that `holosmith doctor --quick` over the *same*
stand-in fails its `disk` line with the same two numbers — the acceptance's "the two never disagree", measured.
It needs `python3` for the `_ping` socket and prints one `SKIP` line without it; `tests/disk.rs` covers both
paths, the turn that settles mid-crossing and `min_free_gb = 0` without a socket (ADR-0068).

…and since HS-063, **the per-session disk soft cap**: `case_disk_quota` boots its own Docker run with a `du`
**stand-in** on `PATH` — a `/bin/sh` script that appends its arguments to a file this case reads and answers a
`du -sk` line over the cap — because the size a real worktree reaches is not something a gate can build, and
because the recorded arguments are the proof that the watchdog measured *that session's* worktree rather than
the data root. It creates one session, waits for `idle` (so the worktree is real: a stub session has none),
**restarts the server**, and lets the watchdog's first pass — which waits for the boot's own re-provisioning to
settle (ADR-0069) — measure it. It asserts the row is `stopped` with `"status_reason":"disk_quota"` and not
`error`; that the stand-in was asked about `<projects>/.worktrees/<id>`; that the `session.status` frame is
`idle → stopped` with that reason; that the stream carries an `error` frame with `"code":"disk_quota"` and the two
numbers (the page's payload is an escaped string, so the page itself is asserted by kind), that
`GET …/events/history` carries that `error` frame; and that `POST …/resume` answers `202` and walks the row back to
`idle` — the acceptance's “resumable after cleanup” measured. It needs the daemon **and** an `agent_image` and
prints one `SKIP` line without them; `tests/quota.rs` covers both ladder rungs, the under-the-cap case, an
unmeasurable worktree, `session_disk_soft_gb = 0` and the measurement's arithmetic without a daemon (ADR-0069).

…and since HS-064, **the stopped-session expiry**: `case_stopped_expiry` boots its own Docker run over a real
repository, because the story's claim is that the daily job runs the **delete flow** — so what survives it has to
be a real branch and a real `refs/holosmith/wip/<id>`. It creates one session, waits for `idle`, commits once on
`agent/<id>` and leaves one file uncommitted (so the branch is **kept work** and the snapshot has something to
write), and creates a second whose row it then writes into a shape no writer of this build produces: `error` with a
stale `stopped_at`. It **backdates** the first row's `stopped_at` eight days — past the default
`stopped_ttl_days` of seven — with `python3`'s `sqlite3`, and **restarts the server**, because the job's first
pass is immediate at boot (ADR-0070) and a gate cannot run a seven-day wall clock. It asserts the session is gone
from `GET /api/sessions`, its branch is still at the agent's commit and its WIP ref still resolves (the run's own
`git rev-parse`), its worktree, session directory and `agents_<id>` network are gone, the `error` row is still
`error` (only the candidate query's status predicate can keep a row that has a stale clock *and* a non-stopped
status, which is the isolation `tests/expiry.rs` cannot build), the list's answer carries
`"stopped_ttl_days":7`, and the shipped bundle carries the three days-remaining sentences — the last bullet
without needing a browser. It needs the daemon, an `agent_image` and `python3` and prints one `SKIP` line without
them; `tests/expiry.rs` covers the pass, the skip-and-retry, the `error` arm, the six reasons into `stopped` and
the refused resume without a daemon (ADR-0070).

…and since HS-066, **the Web Push notification**: `case_push` is daemon-free and stands a real TLS receiver in front
of the sender. It makes a CA and a `localhost` certificate with a `DNS:` SAN, generates the machine's VAPID pair
with the same `openssl ecparam … -outform DER` the deploy runs (converted to base64url with `python3`, written as
`<data>/secrets/vapid.toml`), and starts a `node:https` listener on `localhost:0` that **verifies the token's ES256
signature against that pair** (`crypto.verify` with a `P-256` JWK), **decrypts the body** with the subscription's own
private key, checks the `aud` claim against its own origin, and appends one JSON line per request. The boot runs with
`SSL_CERT_FILE` exported and unset immediately after, because that is how a private CA reaches `openssl s_client` —
the sender passes no `-CAfile` on purpose (ADR-0071). It asserts the store (a subscribe; a re-subscribe of the same
endpoint as one row with the newer `auth` and the original `created_at`, read out of `app.db` by `python3`'s
`sqlite3`; the `422` arms; a `DELETE` that removes the row and then answers `404`), the **stream-open pair** (a
prompt with the session's SSE stream held sends nothing; the same prompt without it sends one message per
subscription, whose payload is `{host, session_id, kind}` with `kind` `budget_exceeded`, `host` `local`, and
`jwt_ok`/`aud_ok` true), and
the **disposition table** at the far end (`/gone/` answers `410` and its row disappears; `/keyed/` answers `403` and
its row stays). It prints one `SKIP` line and passes without `openssl`, `node` or `python3`. What it cannot reach is
why the long-turn and `error` triggers are `tests/push.rs`'s: the stub's turn is a fixed 2.4-second script with no
failure mode over HTTP, and `LONG_TURN` is a constant twenty seconds with no key to shorten it.

…and since HS-067, **the installable app**: `case_pwa` is the other case a **UI** story adds without a browser, and it
measures what a browser would otherwise be the only witness of, as bytes. It boots the artifact on its own data root
with a hand-written VAPID pair, so it needs **no `openssl`, `node` or `python3` and prints no `SKIP` line at all** — a
skip here would fire exactly when the feature was deleted. It asserts `GET /` is this tree's `web/dist/index.html`
byte for byte and names the manifest and iOS's `apple-touch-icon`; `GET /manifest.webmanifest` is this tree's file
served as `application/manifest+json` with `display: standalone`, `start_url`/`scope` `/`, both `any` sizes and the
`maskable` icon; `GET /sw.js` is `web/dist/sw.js` byte for byte, served as JavaScript, **not** the shell (a `/sw.js`
answered by the fallback is a registration every browser refuses, silently) and carries the four `kind` sentences, the
three events and the sixteen-hex id rule; each of the four icons is its file's bytes as `image/png`; `GET /settings`
is that same shell (the third screen needs no route — ADR-0007's fallback answers it); the **served bundle** carries
the screen's ten element ids, the iOS sentence, the per-machine sentence and two state sentences, each needle quoted
as an assignment so a deleted container cannot be covered by a sibling's id; and `GET /api/health` carries the pair's
`public_key` **byte for byte** and the private half nowhere, then `null` once `<data>/secrets/vapid.toml` is removed —
the last measurement being the one that proves a browser could really subscribe. What it cannot see is the tap, the
notification and the install itself: the sentences and the branches are `web/test/push.test.ts`'s, and the demo is
what judges the rest — Chromium's own `Page.getInstallabilityErrors`, a `push` event delivered to the registered
worker, the notification it draws, and the handler's `clients.openWindow("/sessions/<id>")`. An install on a device
and a click on a system notification are the two things no machine here can do, and both are stated as
not-measured (ADR-0072 D9).

…and since HS-068, **the host registry**: `case_hosts` boots the artifact on its own data root with the control
port **pinned** — a row naming *this* machine has to spell the port the boot will use, and the seed is written before
the boot — and stands three TLS listeners in with `python3`: one answering the health payload, one answering a `302`
(and appending a line per request), one answering a payload with no `name`. Their certificate comes from a private CA
that `SSL_CERT_FILE` points at for exactly two `start_serve` calls, which works because `hosts::probe` passes **no**
`-CAfile` (ADR-0071's rule). The seed names those three plus a MagicDNS name that resolves nowhere plus the local
row, and the case asserts what each earned: the healthy host `reachable` with **its own** version, the redirect
`unreachable` with `302` in its sentence and the stand-in **asked exactly once** (that is "redirects are not
followed" as a client can see it), the nameless payload `unreachable` with the missing field named, the unresolvable
name `unreachable` with the host named, and the local row `reachable` with *this build's* version without a round
trip. Then item 4's rules over the wire — the scheme, a host that is neither MagicDNS nor 100.64/10, loopback, two
out-of-range ports, a path, the reserved name `local`, an uppercase name and a fourth body key, each with its own
code — a valid registration stored `unreachable` rather than refused, the local row's `409 host_busy` (with the
count and `?force=true` in the sentence) over a live session, its forced removal leaving the session, a remote row's
delete needing no force, and a `404`. A **second boot** with a changed file then proves both halves of "the file wins
on conflicts": the row the file names is the file's (and its reach flipped), the row the API made and the file does
not mention survives, and the local row the operator removed comes back. Every row is probed at every boot, seeded or
not, so a machine whose registry is only `POST /api/hosts` re-asks its hosts at each start. It prints one `SKIP` line without `python3`
or `openssl`. What it cannot measure through the artifact is the API's `422 not_holosmith` against a live listener —
the host rule refuses loopback and no `.ts.net` name resolves on a machine with no `tailscaled` — so that arm is
`tests/hosts.rs`'s, split into the probe against real listeners and the mapping as a function (ADR-0073 D10).

…and since HS-047, **`server deploy`**: `case_server_deploy` has two halves that skip independently, because the
bugs it exists to catch are of two kinds. The **offline** half needs `python3` and nothing else: a `_ping` socket
it answers stands in for the daemon, and the case asserts that the dry run lists the seven steps in the spec's
order with step 3 planning the two images (`needs`) and steps 5, 6 and 7 planning real work — the unit's path, both
`--https` targets and the `/api/health` wait — that **no** step is `skipped` and that the report has no `deploy:
incomplete` line (ADR-0058), that an install root under `target/` does **not appear** afterwards, that a run whose
`docker_host` names a socket that cannot exist stops at step 1 with the resume command as its last line, and that a
run which gets past step 1 and then fails step 3 (that same socket answers `_ping` and nothing a build's label read
needs) leaves step 2's files on disk, prints the resume command with the resolved `--root`/`--config`, and
evaluates no later step at all. The **online** half needs the daemon and an `agent_image` that exists: it removes
any `hs_egress`/`hs_key` an earlier case left behind — those are the machine's names, and a leftover would make its
own first run read `satisfied` — removes both image tags step 3 builds (so its `changed` is a real build,
layer-cached), then runs a real deploy into a root under `target/` with the `tailscale` stand-in on `PATH` and a
`python3` responder holding the control port, asserting step 2's four paths with their modes (0755, 0644, 0700,
0600, 0700) and the VAPID pair, step 3 `changed`, step 4 `changed` with both containers running, step 5 `changed`,
step 6 `changed` and step 7 `satisfied`, and then the same command again, where **all seven** steps say `satisfied`,
the tree's listing and `config.toml`'s bytes are byte-identical and the shim applied no second origin, and the tail
names the URL and `holosmith key set` (ADR-0056, ADR-0058). Only a failed step exits 1; the `incomplete` line it
used to print went with the steps that were still other stories'.

…and since HS-049, **`case_server_service`** owns the three machine steps from the shipped artifact, in two halves
that skip independently and print one `SKIP` line each. The **offline** half needs `python3` and nothing else: it
builds the install tree by hand with `install -d`/`install -m` — so every mode is the case's own choice rather than
this shell's umask — and reads `doctor --quick`'s `perms` line three times, a `pass`, a `fail` after `chmod 0755` on
the secrets directory naming both `0700` and `0755` and the `chmod` that fixes it with exit 1, and a `pass` once
the mode is back; then it dry-runs the deploy over a `_ping` socket and asserts the plans for steps 5, 6 and 7, that
nothing is `skipped`, and that no `incomplete` line exists. The **online** half needs the daemon, an image and
`python3`: a real `--root` install with the `tailscale` stand-in on `PATH` and the responder on the control port
asserts `2. files: changed:`, `5. service: changed:`, `6. origins: changed:` and `7. health: satisfied:`, the VAPID
file at 0600 with an 87-character `public_key` and a 43-character `private_key`, the unit at 0644 carrying
`Restart=on-failure`, `ExecStartPre=<artifact> doctor --quick`, `ExecStart=<artifact> serve` and
`WantedBy=multi-user.target` and **no** `User=` (that is the system path's half), and the shim's log carrying
`--https=443 http://127.0.0.1:<control>` and `--https=8444 http://127.0.0.1:<preview>`; the second run is all seven
`satisfied`, with the VAPID and unit bytes identical under `cmp` and no second `serve --bg` in the log. The two
stand-ins are shared with `case_server_deploy` and live beside it: `deploy_machine_shims` writes the `tailscale`
shim, which records every argv and answers `serve status --json` from the `<port> <target>` pairs it was asked to
serve, and `health_responder` starts the `python3` listener that holds the control port. What neither half can
measure — a unit systemd loads, a daemon it can ask, a `useradd` it may run — is `tests/deploy.rs`'s and the
by-hand section near the end of this file (ADR-0058).

…and since HS-048, **`case_server_images`** is the one case whose subject is the build itself, and it runs
**first among the cases whose boot starts the proxies** (not the first daemon case: `case_create_session`,
`case_worktree` and the delete case precede it and need the daemon for other reasons): after this story a `serve`
boot cannot start `hs_egress`/`hs_key` without the proxy image, so every later case that starts a server with the
Docker backend depends on the images it leaves behind. It owns the absent state and **asserts it does**: the
deploy's own `hs_egress`/`hs_key` go first — a running container pins the image it was started from, so an untag
with them up fails and the deploy below would rebuild nothing — then both tags, and `docker image inspect` must
answer "no such image" for both before the deploy runs. It asserts the five `org.holosmith.*` labels on the two
images (version, git tag, pinned pi and the binary's SHA-256 on both, compared with the artifact's own `version`
lines and `sha256sum`; the toolchain list on the agent image only); the agent image's contents (`pi --version` is
`pins/pi.txt`, `git --version`, each configured toolchain, `id -u` equal to the **invoking** user's — ADR-0035's
rule, not a base image's literal — and `holosmith container snapshot --help`); the proxy image serving **both**
proxies from one entrypoint with no bind of the host's binary in `docker inspect` (a `403` for a host off the
allow-list, and the key proxy's own refusal for a provider path it does not know); and the same deploy again
saying `satisfied` with both image ids unchanged. It also reads `doctor --full`'s `image` line back, which is the
label comparison through its other caller. It needs the daemon, `python3`, and **outbound network for the first
build** (pi and the toolchains come from two package managers), and prints one `SKIP` line without the first two
— which `tests/images.rs` covers, apart from the network. It **keeps both images** while removing its own
containers and scratch root — `case_server_deploy`'s online half removes the tags *it* built because that case
asserts `changed` (ADR-0057).

…and since HS-051, **the drain** is two cases, because the drain has two subjects: a machine that becomes quiet and
one that does not. `case_drain` owns its boot (three live slots, no `idle_stop_min`, one session left `idle` and one
prompted so there is a turn to wait for) and measures the acceptance bullet by bullet: the socket exists at
`<admin dir>/drain.sock` as the kernel's own `-S`; `POST /api/drain` is the fallback's `404 not_found` and a raw
`drain 600000` written to the **control port** is answered by the HTTP parser as a malformed request, which is the
API table's "not reachable over HTTP" measured from both sides; a `--wait 200ms` against the stub's ~2 s turn exits
**1** with `state: draining`, `waiting: 1 turn still running`, `result: expired` and the sentence saying the machine
is still draining; in that same window the create and the prompt answer `503` `draining` with
`content-type: application/json` while `GET /api/sessions` and `GET /api/sessions/{id}` answer `200` (the row still
`running`); a `--wait 30s` then waits the turn out, prints `stopped: 2 sessions with reason server_restart`, and
both rows are `stopped server_restart 1` in the database — `expected_stop` is polled, because the container's stop
is a task of its own — with `idle → stopped · server_restart` in the session's transcript and one `holosmith::drain`
log line per stop; a second drain says `already draining`, stops `0 sessions` and still exits 0; and a resume is
refused `503` while a delete is answered `202`. It needs `python3` for the two row facts no client is sent
(`status_reason` and `expected_stop`) and it stops and restores the shared boot, because stopping every `idle` row
would take the run's own `$created_id` with it. `case_drain_restart` is the fifth acceptance bullet, which only the
drain that *could not* finish exercises: it expires against a live turn, asserts a create is still `503` **after**
the expiry, `kill -9`s the server, boots again and asserts the row comes back `idle · interrupted` (ADR-0053) and
that the new process is not draining, because a create is accepted. Neither case needs the daemon (the stub's turn
is what they wait for) and both `SKIP` only on a missing `python3`, with `tests/drain.rs` covering the sequence
without one.

One rule for every case above and below: a case that must assert one code keeps every higher guard check correct,
or it asserts another guard's code under its own name.

The CORS sweep is deliberately **not** in that array: it runs once after every case and checks
that no answer the run saved carries an `access-control-*` header or `vary: origin`. Being
outside the loop is what makes it cover a case a later story appends, without anyone having to
remember to place the new case before it.

**Every later story adds at least one `case_*` function and one line in `serve_cases`.** That is what makes
this a growing gate rather than a snapshot: a story that adds a route or a behaviour has one obvious place to
prove the shipped binary does it, and since HS-046 every case prints one `ok <case>` line when it passes — a case
that printed `SKIP` withholds it, so a skip can never read as a pass — which is what makes a green run name what
it proved instead of only counting it. A case that needs a setting the rest of the run must not carry gets a
boot of its own (see `case_token_budget`, `case_usage_survives_resume`,
`case_compaction`, `case_history`, `case_backup`, `case_worktree`, `case_worktree_refusal`,
`case_changes` and `case_merge`, `case_stopped_expiry`), the
Internet-access checkbox (`case_internet_toggle`, described above), and a database from a newer release
(`case_schema_newer`, which needs no daemon: it boots the artifact twice over one data root, rewrites the
recorded `schema_version` to 99 in between with `python3`'s `sqlite3`, and asserts that the second boot's
banner prints `schema 99`, that its one `warn` names both versions and the command that refuses
(`doctor`), and that `GET /api/health` still answers `200` — the "serve continues so a rollback needs no
restore" half of ADR-0047 — and then that `doctor --quick` **exits 1 with the same sentence**, the refusing
half HS-045 built), and takes a place near the end of the array: nothing after it may assume the first boot's
state, and a case that stops the server leaves the run with whatever boot it started (`case_doctor_full` follows
it as the last case, booting a Docker server of its own).

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
- **`openssl` on the host** for Web Push notifications (HS-066): `push::Pusher` signs each VAPID token with
  `openssl dgst -sha256 -sign`, derives each message's secret with `openssl pkeyutl -derive`, generates the
  application server's key with `openssl ecparam` and delivers with `openssl s_client` — four children per message,
  all of them the same host program `server deploy` already requires (ADR-0071). A machine without it still serves
  and still streams every turn: a notification is a channel `doctor --full` reports (`Check::Openssl`) rather than
  something `serve` refuses to start over.
- **`tailscale`** and **systemd** for a deployed server — and they **are** prerequisites of a deploy now: `doctor`
  reports a machine without either as `unknown` rather than failing (a LAN-only machine and a hand-run `serve` are
  supported), and `server deploy` runs `doctor --quick` as its step 1 (HS-047), so such a machine still gets through
  step 1. Step 5 (HS-049) writes the unit and runs `systemctl daemon-reload` and `systemctl enable --now
  holosmith.service`, so a machine without systemd fails that step with systemd's own line, and step 6 configures
  `tailscale serve` for the two origins, so a `tailscaled` that does not answer fails step 6 while `doctor`'s
  `tailscale` finding stays `unknown` on the same machine — the same host, two different questions (ADR-0054,
  ADR-0058).
- **`install`, `chmod`, `id`, `stat`, `useradd`, `usermod` and `chown`** (coreutils, plus the account tools
  `useradd`/`usermod`), **`openssl`**, and
  **`sudo`** on a machine whose paths are the system's, for `holosmith server deploy` (HS-047, HS-049): every file
  action is one `install`/`chmod` child, and it is prefixed by `sudo` unless `--root` builds the tree as the
  invoking user (ADR-0056); step 2 reads `id -u`/`id -nG` and `stat -c %u` to decide whether the service user and the
  tree's ownership are already what the unit's `User=` needs, and writes them with `useradd`/`usermod`/`chown`; and
  `openssl ecparam -name prime256v1 -genkey -noout -outform DER` is what generates the machine's VAPID pair — the
  same `openssl` the key proxy spawns at runtime, needed by the deploy as well as by the key proxy (ADR-0048,
  ADR-0058). The gate's **case** needs none of them: it installs under `target/` with `--root`, so no step takes the
  privileged path. `cargo test` runs `tests/deploy.rs`, which drives the host's `install` and `chmod` for real (by
  name, through `PATH`) to assert the modes a real install produces, and asserts the
  `useradd`/`usermod`/`chown`/`systemctl` argv through recorders that execute nothing. That is the same coreutils
  `preflight` already relies on for `df`.
- **git** for `scripts/build.sh`, and for a Docker run at runtime: the worktree step runs
  `git worktree`, `git rev-list` and `git branch` against the project repository on the host
  (HS-025). Not required by `cargo build` itself, and not required by `--backend stub`.
- Optional: **`python3`** for the parts of the gate that read a database or speak a socket by hand:
  `case_worktree` (answers the Docker preflight from a socket saying `200 OK`, so a Docker boot can be driven
  on a machine with no daemon), `case_compaction` (ages a session's event log with stdlib `sqlite3`, because
  the compaction window is the spec's fixed 24 hours rather than a config key — ADR-0032),
  `case_exit_classification` and `case_usage_survives_resume` (read a column the API does not carry), the
  history case, `case_schema_newer` (writes a `schema_version` from the future), and `case_doctor_quick`
  (answers `_ping` on a socket, picks the two free ports it asserts and holds one of them to fail the port
  check). Without it each case prints one `SKIP` line and the run passes; `cargo test` covers the behaviour
  either way.
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
- Optional: a **Docker daemon and one image** for `scripts/verify.sh`'s `case_container_mounts`,
  `case_reconcile_orphans` and for `tests/container.rs`. Both check V1 — the hardened `.git` mounts — against a real container, and both
  print one `SKIP` line and pass without a daemon, because a machine without one is a supported machine
  (a hub). The image is `$HOLOSMITH_TEST_IMAGE`, default `pi-agent:local`; it needs `sh`, `git` **and
  `node`** — the network self-test's probe is a node script (HS-036, ADR-0044), and an image without node makes
  that test `Unknown` rather than failed, so a missing node is a `warn` at every boot instead of a red test. Its
  default user must not be root. Those cases run it as the config's `agent_image`, which is why a development
  machine may point that key at an image it tagged by hand instead of at the name `server deploy` builds.
  **The machine's two proxies no longer run that image** (HS-048, ADR-0057): they run
  `holosmith-proxy:<version>`, which step 3 builds from the Dockerfile embedded in the binary — the artifact on
  `alpine` plus `openssl` and CA certificates, because `hs_key` spawns `openssl s_client` (ADR-0048). A `serve`
  boot on a machine that has never deployed starts no proxy: one `warn` naming `holosmith server deploy`, and a
  session that allows internet access reaches nothing through it.
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
fallback is not in use. The image is the config key `agent_image` (default `agent-image:<version>`, the name
`server deploy`'s step 3 builds), and its contents are the story's other half: pi at the pinned version, git,
the toolchains the key `agent_toolchains` names (default `["python3", "ripgrep"]`; an empty list is a machine
whose projects need only git), and this binary for `container snapshot` and the proxies (HS-048, ADR-0057).
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
results are written into `HolosmithSpec.md`'s verification table, and the gate keeps only the half that needs
no key (`case_key_proxy` sends a request through the proxy with a stand-in `openssl` on `PATH`, and asserts the
bytes the proxy produces: one `Content-Length`, the body after the blank line, and the stored key swapped in).
The table's V3, V4 and V5 cells name this section, which is what makes them a procedure rather than a claim —
`case_verification_table` fails the gate if a cell names a file this repository does not have.

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

## Deploying a real host by hand

`server deploy` is the supported path, and on a real host it is a handful of commands for an operator who has the
artifact and `sudo`. **Copy the artifact to a stable path first**, because step 5's unit names the path of the
binary that wrote it (`std::env::current_exe()`, ADR-0058) — a deploy run out of `target/debug` writes a unit
pointing at `target/debug`, which `cargo clean` removes and which systemd could not read anyway.

```bash
scripts/build.sh
sudo install -m 0755 dist/x86_64-unknown-linux-musl/holosmith /usr/local/bin/holosmith
/usr/local/bin/holosmith server deploy --dry-run    # the plan, and it creates nothing
/usr/local/bin/holosmith server deploy             # each write asks `sudo` for what it needs
```

Run it as yourself and let the `sudo` children ask: that is the shape ADR-0056 records, and the child inherits the
deploy's stdin so a password prompt lands on your terminal. `--root` is for a tree you own and is what the gate
uses; on a real host the deploy takes the system paths. The config it writes is `/etc/holosmith/config.toml`
rendered from the defaults, so edit it **before** the first deploy if `projects_root`, the listen addresses or
`docker_host` are not what this machine wants, and then run the deploy: step 2 rewrites that file when its
rendering differs, and never touches `secrets/keys.toml` or `secrets/vapid.toml`.

What each step does here: **2** creates the `holosmith` system user in the `docker` group, the data directory,
`config.toml`, the 0700 secrets directory with its 0600 `keys.toml` and the VAPID pair in `vapid.toml`, and the
admin socket directory, then `chown`s the tree to the service user, because `serve` runs as that user; **3** builds
`holosmith-proxy` and `agent-image` from the Dockerfiles embedded in the artifact, and a first build pulls pi and
the toolchains over the network; **4** creates the `egress` network and starts `hs_egress`/`hs_key` with
`--restart unless-stopped`; **5** installs the unit, runs `systemctl daemon-reload` and then `systemctl enable
--now holosmith.service`; **6** asks `tailscale serve` for the control origin on 443 and the preview origin on 8444;
and **7** waits for `/api/health` and then runs `doctor --full`, so the run ends with the URL and the next steps
(`holosmith key set <provider>`, and registering the machine in a hub).

Those are the two origins the story's demo names — the control origin on 443 and the preview origin on 8444, on
this machine's tailnet name — plus the one thing a deploy cannot do, which is survive a reboot. So reboot once and
check that it did:

```bash
sudo reboot
systemctl status holosmith.service             # active (running), and enabled
systemctl is-enabled holosmith.service         # what step 5's read compares with
sudo tailscale serve status                    # both origins, with their targets
curl -fsS https://<machine>.<tailnet>.ts.net/api/health
sudo holosmith doctor --full                   # as root, because `perms` reads the 0700 secrets directory
```

`doctor --full` is the same list the unit's `ExecStartPre` runs, plus the checks that only mean something on a
serving machine: the network self-test, both proxies, the image's pi version, and a held port that answers
`/api/health` read as the pass. A `perms` failure there names the mode, the mode it should be and the `chmod` that
fixes it — and because it is `doctor --quick`, the same failure is what stops the unit from starting, so a reboot
that reports `activating (auto-restart)` is a mode or an owner rather than a mystery.

**What this repository's environment could not measure**, and therefore what this procedure is for: there is no
systemd here (so no unit was ever loaded and no boot happened), no `tailscaled` (so the two origins existed only in
the gate's stand-in answer), no `useradd` (so the service user, the three `chown`s and `systemctl enable --now` are
`tests/deploy.rs`'s recorders) and no reboot. The plan, the unit's bytes, both `tailscale` argvs, the VAPID pair's
identity across two runs and `doctor`'s `perms` arms are measured; "reachable over HTTPS" is this section, run on a
machine that has the three programs this one does not (ADR-0058).

## Watching and tearing a host down: `status`, `logs`, `uninstall`

Three commands an operator runs *after* a deploy, and the reason they exist is that none of them should need a
systemd incantation or a hand-written `docker` call (HS-050, ADR-0059).

```bash
holosmith status                     # one `key value` line per fact
holosmith status | grep '^uptime'    # works on a machine that is not serving: it says `unknown:` and why
holosmith logs                       # journalctl -u holosmith.service, all of it
holosmith logs -f                    # ... and follow
holosmith logs --session <id>        # <data>/sessions/<id>/session.log
holosmith logs -f --session <id>     # ... and follow the file
```

`status` prints `role`, `version`, `uptime`, one `sessions <status>` line per session status, one `proxy <name>` line
per proxy, and `machines`. **`uptime` is the running server's**, read from `GET /api/health` on the config's
`control_listen`: a machine that is not serving prints `uptime unknown: …` rather than a number from `/proc` or from
systemd, and that is also how it says "not serving". `machines` is `unknown:` with a sentence naming HS-068 (phase 3)
— there is no host registry yet, and the line says so instead of printing a zero. The exit code is 1 only when a
fact could not be **read** (an unreadable database); a stopped server is a report, not a failure.

`logs` reads journald through one child and a session's log through one file. The session log is the one the exit
classifier wrote (the launch line, and a failed container's stderr tail); `-f` follows either, and an id that is not
a session and an id with no log yet are one sentence each. No `-n` is passed to `journalctl`: any window would be a
truncation the command cannot label, so journalctl's own pager shows everything and `journalctl -u holosmith.service
--since …` is the operator's own filter.

```bash
holosmith uninstall                  # the unit, hs_egress, hs_key and the egress network; the data stays
holosmith uninstall                  # ... again: every step reads first, so the second run says `nothing`
holosmith uninstall --root /tmp/tree # the same over an install tree an unprivileged run made
holosmith uninstall --purge          # also the data root; refuses while a session is live
holosmith uninstall --purge --force  # ... and over a live session
```

What it removes, in order: the unit (`systemctl disable --now holosmith.service`, the file, `systemctl
daemon-reload`; a `systemctl` that answers non-zero over an unloaded unit is a **remark**, not a failure), then
`hs_egress` and `hs_key` (`docker rm --force`), then the `egress` network. What it **keeps**: the data root — the
database, every session's log and `secrets/` with the provider keys and the VAPID pair — the projects with their
worktrees and branches, and the sessions' own containers and `agents_*` networks, which belong to the rows the kept
data describes and are put right by the next boot's reconcile. `--purge` removes the data root and, first, the
`s_<id>` containers and `agents_<id>` networks the rows named; never a branch, a worktree or an image. The
refusal over a live session happens **before anything is removed** and prints the count and the `--force` that
overrides it, because a run that removed the unit and then refused would leave the machine half uninstalled.

**What this repository's environment cannot measure**, and what the by-hand procedure above is for: there is no
systemd here, so no unit is loaded and `disable --now`/`daemon-reload` are `tests/uninstall.rs`'s recorders (and a
privileged run's unit *file* lives in `/etc/systemd/system`, which an unprivileged test cannot write); `journalctl`
exists here but nothing runs under the unit, so the server arm is measured against a stand-in; and there is no host
registry, which is why `status`'s `machines` line names HS-068.

## Draining a machine before a restart

Spec item 10 step 3: an upgrade must not kill a turn somebody is watching. The drain is the command that makes that
possible, and it is deliberately **not** a shutdown — it refuses new work, waits for the turns that are running,
stops the sessions that are merely idle, and then says so while `serve` keeps answering reads (HS-051, ADR-0060).

```bash
holosmith drain                 # drain, waiting up to the default 10m
holosmith drain --wait 1m       # a shorter wait
holosmith drain --admin-dir /tmp/hs/run/holosmith   # a run that is not the machine's
```

One request line goes to a **unix socket** — `/run/holosmith/drain.sock` on a deployed host, `<admin dir>/drain.sock`
otherwise — and the answer is printed as it arrives:

```text
drain: state: draining
drain: waiting: 1 turn still running
drain: stopped: 2 sessions with reason server_restart
drain: result: drained
```

**The exit code is the answer**: 0 only for `result: drained`. `expired` means the `--wait` ran out with a turn
still running, and it exits 1 *and changes nothing* — the machine stays draining, no session is stopped, and the
line says so, because a drain that quietly cancelled itself is the one outcome a promotion must never mistake for
success. `refused` covers a socket that is not there, a line that is not a drain, and a machine that went away
mid-drain.

What keeps working while it drains: every `GET` (the UI must be able to watch the turn being protected),
`abort`, `steer` (it refuses unless the row is `running`, so it cannot start work) and `DELETE`. What is refused,
with ADR-0008's envelope and `503` `code: "draining"`: `POST /api/sessions`, `POST …/prompt` and `POST …/resume`.
What ends the process is still `SIGTERM`, not the drain; the restart the drain precedes is HS-078's `upgrade`.

The socket's protection is the directory's: `/run/holosmith` is created **0700** by `server deploy`'s step 2 and
chowned to the service user, and the socket is 0600, so being able to open it is the authorisation and no route
exists for anything else. A `serve` that finds a **dead** socket (a killed process's) unlinks it and binds — a
`kill -9` must not wedge the next boot — while a **live** one is another server's and this run warns and serves
without an admin socket rather than stealing its clients.

On a host with systemd, by hand, with a turn running:

```bash
holosmith drain --wait 2m && echo quiet || echo not-quiet   # 0 and `quiet` only when the turn finished
curl -s -o /dev/null -w '%{http_code}\n' -X POST localhost:8080/api/sessions   # 503 while draining
curl -s localhost:8080/api/sessions | head -c 200                             # 200: reads still answer
journalctl -u holosmith.service | grep 'holosmith::drain'                     # one line per stopped session
systemctl restart holosmith.service                                          # the operator's move, not the drain's
```

**What this repository's environment could not measure**: there is no systemd here, so `/run/holosmith` is only the
deployed path and every demo and gate case uses `--admin-dir` under a scratch tree; and nothing consumes the drain
yet, because `upgrade`/`rollback` is HS-078/HS-079. The socket, the four report lines, the three refusal handlers,
the rows' `server_restart`/`expected_stop` facts and the restart's `idle · interrupted` are measured — by
`case_drain`, `case_drain_restart`, `tests/drain.rs` (the sequence, the bind rules, the request grammar and the
refusals over the real `App`) and `tests/drain_cli.rs` (the printed lines and the exit code against a hand-written
socket).

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
