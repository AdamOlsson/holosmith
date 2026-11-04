# RESUME — orchestrator handoff (cold start, after twenty-five stories)

You are the **orchestrator for implementing Holosmith**. `PROMPT.md` is the standing operating
procedure and still governs; this file is the state of the world — what exists, what the environment
already has, which decisions are settled, and the landmines that cost real time. Read all of it before
touching anything.

The run is **fully automatic**. The operator is not present. Never ask them a question: decide
canonically, record the decision in the Linear issue and the commit message, and continue.

---

## 0. What this project is

`HolosmithSpec.md` is the authoritative specification. A single-operator system that runs coding agents
in containers on machines you own, driven from a phone. The work is sliced into **80 stories**
(`HS-001` … `HS-080`) in `docs/stories/`, one Linear issue each, in five phases:

| Phase | Stories | File |
| --- | --- | --- |
| 0 — walking skeleton | HS-001 … HS-006 | `docs/stories/phase-0-walking-skeleton.md` |
| 1 — session lifecycle (stub backend) | HS-007 … HS-023 | `docs/stories/phase-1-session-lifecycle-stub.md` |
| 1b — Docker backend | HS-024 … HS-046 | `docs/stories/phase-1b-docker-backend.md` |
| 2 — deployable MVP | HS-047 … HS-067 | `docs/stories/phase-2-deployable-mvp.md` |
| 3 — multiple machines | HS-068 … HS-074 | `docs/stories/phase-3-multiple-machines.md` |
| 4 — self-development | HS-075 … HS-080 | `docs/stories/phase-4-self-development.md` |

`AGENTS.md` carries the repository rules, including that **Linear scope is exactly one project**:
`Holosmith` (`P-PER-2`, team `Personal`/`PER`). `VulkanRenderAPI` belongs to another repository — never
read, create, comment on, or close anything there.

**The MVP line** is phase 2's first stories plus, before them, the rest of phase 1: a session can be
created, prompted, aborted, steered, stopped, resumed and deleted, and it is driven from the phone.

---

## 1. Where the work stands

Branch **`master`** (this repo has **no `main`**; do not rename it). The tree is clean, and every
commit is **pushed** (see the note on pushing below). `HEAD` is `ce4dc82`; any commit after it is
bookkeeping — this file's own refresh.

**Linear: 25 Done, 55 Backlog, all in project `Holosmith`.** Done is `HS-001` … `HS-025`
(`PER-33` … `PER-57`).

| Story | Linear | Commits | ADR |
| --- | --- | --- | --- |
| HS-001 static binary, `version` | PER-33 | `e481ea7`, `d302635` | 0004, 0005 |
| HS-002 one typed config | PER-34 | `bbb1f0f`, `a45edac` | 0006 |
| HS-003 `serve`, embedded app, `/api/health` | PER-35 | `50c20ee`, `e71da58`, `cc9aadb` | 0007, 0008 |
| HS-004 SQLite + forward-only migrations | PER-36 | `a4774b9`, `bd3b73e`, `ae92a7d` | 0009 |
| HS-005 browser-facing guards | PER-37 | `6c4f02c`, `6994c0f` | 0010 |
| HS-006 `scripts/verify.sh` | PER-38 | `2af15d4`, `108a6ee` | — (0003 governs) |
| HS-007 session row + repository, schema 2 | PER-39 | `ad3ffc0`, `fad974d` | 0011 |
| HS-008 `POST /api/sessions` | PER-40 | `c5891e4`, `858d295` | 0012 |
| HS-009 event log + single writer, schema 3 | PER-41 | `5cae107`, `dae1fd9` | 0013 |
| HS-010 SSE stream, `Last-Event-ID` | PER-42 | `3b03c5c`, `07f318d` | 0014 |
| HS-011 stub backend + provisioner steps | PER-43 | `9071390`, `b5022fc` | 0015 |
| HS-012 one enforced state machine | PER-44 | `9424677` | 0016 |
| HS-013 sessions list + new-session form | PER-45 | `47471c4` | 0017 |
| HS-014 session screen, live transcript | PER-46 | `82c8c35` | 0018 |
| HS-015 prompt and the turn lifecycle | PER-47 | `d4077df` | 0019 |
| HS-016 abort with the full ladder | PER-48 | `d034237`, `3d2ecba` | 0020 |
| HS-017 steer a running turn | PER-49 | `0099ca3`, `8ff1d77` | 0021 |
| HS-018 stopped state and resume | PER-50 | `dce53c4`, `ff9d907` | 0022 |
| HS-019 delete flow | PER-51 | `3d5c415`, `7c4ea02`, `cd85572`, `db95a3f` | 0023 |
| HS-020 cap and eviction | PER-52 | `a3c6825`, `4f37211`, `f67eb6f` | 0024 |
| HS-021 idle stop | PER-53 | `5da2ae2` | 0025 |
| HS-022 turn timeout | PER-54 | `e767757` | 0026 (amends 0020) |
| HS-023 token budget | PER-55 | `2e902c2`, `fbc0659` | 0027 |
| HS-024 Docker preflight | PER-56 | `f241ced`, `0b7d50e` | 0028 |
| HS-025 worktree provisioning | PER-57 | `7e36bde`, `1fe7f6e`, `ce4dc82` | 0029 |

**Next issue: `PER-58` (HS-026 — worktree refusal rules).** Then onward in `HS-###` order. §8 carries
what HS-026 is about and the facts it inherits.

Ranking, per `PROMPT.md`: Linear priority (all are `No priority`) → `HS-###` order in `docs/stories/` →
anything already `In Progress` first → skip an issue whose `Blocked by` stories are not `Done`.

### Eight facts a cold start must know

1. **A running turn can now be stopped, in two rungs, with or without the way back.** `src/ladder.rs`
   is the shared routine: `clear_queue` then `abort`, wait up to `abort_grace` (the grace *bounds rung
   1 too*), then `running → stopped` with the caller's reason and `stop`. `Ladder::climb` is that plus
   a resume through the provisioner (the abort's path, unchanged); `Ladder::end_turn` is that alone and
   returns `Ending::{Asked, Forced}` (HS-019's delete, which then removes the session instead of
   resuming it). A turn's end is typed (`TurnEnd::{Settled, Aborted}`) so the runner picks
   `agent_settled` or `abort_completed`; a forced stop has **two owners** and the table decides, which
   is why exactly one `StateError::Illegal` in `turn.rs` is a `debug` line. **Two policies now climb it**
   — HS-022's deadline and HS-023's ceiling — and both sit in the runner's own `select!`, each guarded on
   one `fired` so the first to fire owns the reason and no second climb is spawned. The six reason words
   are `forced_abort`, `deleted`, `evicted`, `idle_stop`, `turn_timeout` and `budget_exceeded`.
2. **A turn that will not end is ended for it, and the reason says which policy asked.** The deadline is
   the turn runner's own (`Turn::run`), not a sweeper's, because the row keeps no per-turn timestamp —
   `last_active_at` is activity, so a steer moves it. It races `start_turn` against a sleep and, when the
   sleep wins, hands the ladder a **task of its own** (`Ladder::spawn`) and then keeps awaiting the turn,
   because rung 1 working *is* the agent's `TurnEnd::Aborted`. **A `select!` branch instead of a spawned
   task is a bug**: it drops the climb, and a climb dropped between rung 2's `stopped` and its resume
   leaves a session nobody can bring back. Both endings record `turn_timeout` — the runner's
   `running → idle` and the ladder's `running → stopped`. `turn_timeout_min = 0` builds no timer at all.
   It is the shape HS-023's budget copies: one `fired`, a spawned climb, and a settle that records the
   reason too when it crossed the line between two polls.
3. **The web page has now been rendered, including the paths a frame changes.** Playwright's Chromium
   is inside the sandbox (see §2), and HS-016 … HS-018 drove the sessions list, the create form, the
   session screen, the composer's four actions, the transcript's blocks and boundaries, and the abort /
   steer / resume round trips; HS-019 drove the delete's 202 → 404 and the closed stream; HS-020 drove
   the cap's eviction **both ways** — the row as drawn from the list, and a row turning `stopped` under
   the operator from a `session.status` frame with no reload (which is where that story's one blocker
   lived); HS-021 drove the sweeper's frame path the same way (a row drawn `idle`, then turned
   `stopped · idle_stop` under the operator with no reload). This is still **not** a substitute for a
   gate — nothing in `scripts/` may require a browser —
   but "unverified from source" is no longer the honest answer for a screen that has been looked at.
   What has *not* been looked at: the hub (HS-057+), the changes sheet, and anything phase 2 adds.

4. **A quiet session stops costing money, on a timer nobody asks for.** `sweeper.rs` reads every `idle`
   session whose `last_active_at` is older than `idle_stop_min` (default 30; **`0` disables it**), applies
   `Trigger::IdleStop` with reason `idle_stop`, logs one `info` line per stop, and spawns the backend's
   `stop`. The tick is a **constant** (5 s), not a config key, so the gate pays a real minute for
   `case_idle_stop`. Only `idle` is a candidate, and the repository's `idle_before` carries that predicate.

5. **A session's spend is a column the event writer adds to, and the runner is what trips the ceiling.**
   `message.end`'s `usage` is added to `sessions.usage_in`/`usage_out` inside `insert_batch`'s own
   transaction (the one that assigns the seq), because the *backend* emits the frame and the runner never
   sees it. `max_session_tokens` (unset = unlimited) is the ceiling; the runner polls the row's
   `usage_in + usage_out` every 100 ms and climbs the ladder with `budget_exceeded` at **or over** it. A
   turn that settles over its ceiling records the reason on its own ending instead (there is nothing left
   to abort), after one extra look a writer tick later. A backend that reports no usage leaves nothing to
   count, and the session screen says so in one sentence (`budgetNotice` in `transcript.ts`) rather than
   showing a zero. The tally survives a resume; that is the spec's "per session".

6. **A session's worktree is real now, and the row records where it is.** `src/worktree.rs` holds spec
   item 1's five host commands as one value (`repository`, `path`, `branch`) with `ensure`, `ahead` and
   `remove`; the Docker backend runs them on `spawn_blocking` and answers
   `Provisioned::Worktree { path, base_commit, created }`; the *provisioner* writes the row, because a
   backend may not (ADR-0019). `ensure` is create-**or-adopt** (a resume re-provisions a worktree that is
   still there) and the row's `base_commit` wins over the branch's head. Teardown treats a missing
   worktree, branch *and* project repository as success, so a delete cannot wedge on ADR-0023's `deleting`
   row. **A step that only adopted is not undone** when a later step fails (`Provisioned::undoable`):
   those commits are the agent's, and deleting them would be data loss dressed as cleanliness.

7. **The step after the worktree is still the one that refuses**, so a Docker-backed create ends `error`
   with `the network step: … (HS-027)` and the provisioner takes the worktree back. That is why
   `case_worktree` asserts the worktree is *gone*, and why the story's "commit in it, see `ahead`
   increment" half is only shown by `tests/worktree.rs` (ADR-0029 records this). The `Session` value a
   later step is handed is the **create's**, so `session.worktree` is still `''` even after the row has
   the path — HS-027's mounts must derive it (`DockerBackend::worktree`) rather than read it.

8. **A Docker run does not start on a machine that cannot carry it.** `src/preflight.rs` checks the
   daemon's `_ping` over `docker_host`, git ≥ 2.38 and `df -Pk <data root>` against `min_free_gb`, and
   `serve --backend docker` refuses **before the database is opened** with one sentence per failure that
   names the fix (all failures at once). `role = "hub"` is exempt — a hub runs no sessions (item 10 §6) —
   and so is `--backend stub`, which the gate and `scripts/check-static.sh` use. `Complaint` is the type
   HS-045's `doctor` will print. `tests/backend_contract.rs` is the conformance suite both backends must
   pass; each Docker story adds its case.

Three things HS-014 and HS-015 settled that later stories inherit:

- **The transcript's event contract** (ADR-0018 + ADR-0019), emitted by the backend:

  | Kind | Payload |
  | --- | --- |
  | `message.delta` | `{"message_id", "kind": "text"\|"thinking", "text"}` |
  | `message.snapshot` | `{"message_id", "text", "thinking"}` (nothing emits it yet; HS-060) |
  | `message.end` | `{"message_id", "usage": {"in", "out"}}` |
  | `tool.start` / `tool.end` | `{"tool_id", "name", "arguments"}` / `{"tool_id", "name", "output"}` |
  | `turn.start` / `turn.end` | `{}` |
  | `turn.steer` | `{"text"}` — **no message id**: one frame, one block, nothing continues it (ADR-0021) |
  | `session.status` | `{"from", "to", "step", "reason"}` (ADR-0016, the transitioner only) |

  A message id keys a transcript block; a delta is one appended text node; a snapshot is the only
  replacing operation and is the last frame for its id; an unknown kind is dropped, not refused. The
  steer frame is the one deliberate exception: its block is created per frame.

- **The backend emits a turn's frames; the server owns every state change.** `start_turn` takes
  `&EventWriter` for that reason, and no backend can reach a `Transitioner`. The prompt's
  `idle → running` is the *handler's* (so the 202 can promise `running`); the way out (`agent_settled`,
  or `unexpected_exit`) is `turn.rs`'s, and an abort's is `turn.rs`'s or the ladder's. Refusals are one
  family: `409 session_<state>` for **all six** states — `session_starting`, `session_idle`,
  `session_running`, `session_stopped`, `session_error`, `session_deleting` — built by
  `ApiError::not_in_state(status, sentence)`, where the **sentence is the caller's** so an abort's
  refusal does not read like a prompt's. Three refusals are outside that family and each has its own
  constructor: `unsaved_work` (a delete whose snapshot failed), `unknown_session` (`not_found`
  with the session's own sentence — a route that exists and a row that does not, HS-019) and
  `cap_without_idle` (`cap_reached` with the create's sentence — the machine is full *and* nothing is
  idle, HS-020).

**Pushing works, through the sandbox's ssh-agent — but the sandbox's own `~/.ssh/config` gets in the
way.** `origin` is `git@github.com:AdamOlsson/holosmith.git`; `bwrap.sh` binds the host's
`SSH_AUTH_SOCK` at `/run/ssh-agent.sock` (the private key never enters the sandbox), so a push signs on
the host. The bound `~/.ssh/config` has a `Host github.com` block with `IdentityFile ~/.ssh/github`, and
that file is **not** in the sandbox, so a plain `git push` fails with
`no such identity: /home/adamolsson/.ssh/github: No such file or directory` / `Permission denied
(publickey)`. It is intermittent — it worked once and failed the next commit — so use the override every
time:

```bash
GIT_SSH_COMMAND='ssh -F /dev/null -o IdentityAgent=/run/ssh-agent.sock \
  -o StrictHostKeyChecking=accept-new -o User=git' git push origin master
```

`-F /dev/null` is the load-bearing part: it stops ssh reading the sandbox's `~/.ssh/config` at all, which
is what names the missing key. (A plain `git push` worked once and then stopped; `-c
core.sshCommand='ssh -o IdentityAgent=…'` was not enough either — the config still won.) `ssh -T
git@github.com` with those options answering `Hi AdamOlsson!` is the way to check the agent before
blaming the remote.

**The reliable form offers ONE key.** The agent holds three, GitHub rejects the first, and the failure is
intermittent even with the options above. Pin the key that GitHub accepts (the second one here) with
`IdentitiesOnly`:

```bash
ssh-add -L | sed -n '2p' > /tmp/pushkey.pub
GIT_SSH_COMMAND='ssh -F /dev/null -o IdentitiesOnly=yes -o IdentityFile=/tmp/pushkey.pub \
  -o IdentityAgent=/run/ssh-agent.sock -o StrictHostKeyChecking=accept-new -o User=git' \
  git push origin master
```

(`-i` names a `.pub` that is in the agent: ssh offers exactly that one and signs with the agent's copy.)
`git ls-remote` with the same options is the cheap way to tell "pushed" from "the agent refused again".
Confirmed on `origin/master`: `b49f2cd` … `0b7d50e`. Push after every story, and if a push fails, report
it — do not fake one.

---

## 2. Environment (already repaired — do not redo)

Everything below lives **outside the repo** and survives. If this is a fresh container, redo it exactly
as written.

```bash
. ~/.pi/toolchains/env.sh      # ALWAYS source this before cargo/rustc
```

It sets `RUSTUP_HOME`, `CARGO_HOME` and `PATH` to:

- rustup 1.99.1, minimal profile + clippy/rustfmt, targets `x86_64-unknown-linux-musl` and
  `aarch64-unknown-linux-musl`;
- musl.cc cross toolchains for both arches (`x86_64-linux-musl-gcc`, `aarch64-linux-musl-gcc`);
- `~/.pi/toolchains/bin/cc` → `/usr/bin/gcc` (the `cc` crate needs a `cc` on PATH);
- `~/.pi/toolchains/bin/qemu-aarch64-static` (7.2.0) — an **optional dev tool** so the aarch64 artifact
  can actually be *run* here. No repo script may download it; `scripts/check-static.sh` prints one
  `SKIP` line when it is absent and still passes.

Why outside the repo: `/` is a 3.4 GB tmpfs and `~/.pi` is the only large writable ext4 mount.

**Available:** node 24.22.2 / npm 11.19.1 with the npm registry reachable (node 24 strips TypeScript
types natively, which is what `npm test` relies on); `curl`; `python3` (with stdlib `sqlite3` and
`http.client`); `sed`, `grep`, `tr`, `cut`, `cmp`, `find`, `timeout`, `git`; **Playwright's Chromium**
(`~/.cache/ms-playwright`, bound read-only by `bwrap.sh`) and the `playwright` npm package at
`~/.cache/hs-browser/pw/node_modules`. Two quirks, both hit: ESM **ignores `NODE_PATH`**, so a script
imports the package by its own path — `…/playwright/index.mjs` (the `.js` entry is CJS and its default
export does not have `chromium` at the top level); and Chromium must be launched with
`--no-sandbox`, because its own namespace sandbox cannot nest inside ours. A third, learned in HS-020:
**`networkidle` never settles** on the sessions list, because a live row holds an `EventSource` open —
navigate with `domcontentloaded` and wait for the rows.
**Not available, and not to be invented:** no Docker daemon, no `sudo`, no `apt`, no `gh`, no ssh key
or token. There is a **browser now** (see §1 fact 2), but no *gate* may need one.
**No `awk`** and **no `shellcheck`** on this machine — do not write a script or a gate that needs
either.

### Linear access

There are no Linear tools in the tool list. `list_issues` / `save_issue` / `save_comment` / `get_issue`
are reachable through the same HTTP MCP endpoint `scripts/import-linear.py` uses, so drive it from a
wrapper. Recreate `/tmp/lin.py` if it is gone (`/tmp` is ephemeral):

```python
#!/usr/bin/env python3
# Thin CLI over the Linear MCP server, reusing scripts/import-linear.py plumbing.
#   python3 /tmp/lin.py issues            # id, title, status
#   python3 /tmp/lin.py issue PER-48      # full issue JSON
#   python3 /tmp/lin.py comment PER-48 FILE
#   python3 /tmp/lin.py state PER-48 "In Progress"
#   python3 /tmp/lin.py states            # the team's states
#   python3 /tmp/lin.py call get_issue '{"id":"PER-48"}'
import importlib.util, json, sys
REPO = '/home/adamolsson/projects/holosmith'
spec = importlib.util.spec_from_file_location('imp', REPO + '/scripts/import-linear.py')
imp = importlib.util.module_from_spec(spec); spec.loader.exec_module(imp)
PROJECT = 'Holosmith'
a = sys.argv[1:]
if a[0] == 'issues':
    for i in imp.call('list_issues', {'project': PROJECT, 'limit': 250})['issues']:
        print(i['id'], i['status'], i['title'][:70])
elif a[0] == 'issue':
    print(json.dumps(imp.call('get_issue', {'id': a[1]}), indent=2))
elif a[0] == 'comment':
    print(imp.call('save_comment', {'issueId': a[1], 'body': open(a[2]).read()})['id'])
elif a[0] == 'state':
    imp.call('save_issue', {'id': a[1], 'project': PROJECT, 'state': a[2]})
    print(imp.call('get_issue', {'id': a[1]})['status'])
elif a[0] == 'states':
    print(json.dumps(imp.call('list_issue_statuses', {'team': 'PER'}), indent=2))
elif a[0] == 'call':
    print(json.dumps(imp.call(a[1], json.loads(a[2])), indent=2))
```

Always run it with `PYTHONDONTWRITEBYTECODE=1` and `rm -rf scripts/__pycache__` afterwards, or you will
leave an untracked `scripts/__pycache__/` behind. `list_issues` on this project returns **80 issues**
(not 250); `grep -v ' Done '` is the quickest way to the unfinished ones.

**Two API traps, both hit already:**

- `save_issue` takes `state` (the **name**, e.g. `"In Progress"`). It rejects `stateId`.
- `get_issue` takes only `id`. Passing `project` is a validation error.

The team's states are `In Review`, `Backlog`, `Canceled`, `Duplicate`, `Done`, `Todo`, `In Progress`.
Never invent a state name.

**Token expiry is routine, not a blocker.** When a call prints `The Linear access token has expired.
Re-authenticate 'linear' in pi.`, refresh it in place — the store keeps a refresh token and a client id:

```python
import json, os, time, urllib.request, urllib.parse
p = os.path.expanduser('~/.pi/agent/mcp-auth.json'); key = 'mcp__linear|https://mcp.linear.app/mcp'
d = json.load(open(p)); e = d[key]
body = urllib.parse.urlencode({'grant_type': 'refresh_token',
                               'refresh_token': e['tokens']['refresh_token'],
                               'client_id': e['clientInformation']['client_id']}).encode()
req = urllib.request.Request(e['discovery']['authorizationServerMetadata']['token_endpoint'],
                            data=body, method='POST',
                            headers={'Content-Type': 'application/x-www-form-urlencoded'})
new = json.loads(urllib.request.urlopen(req, timeout=60).read())
e['tokens'] = {**e['tokens'], **new}
e['tokensExpireAt'] = int((time.time() + int(new.get('expires_in', 3600))) * 1000)
json.dump(d, open(p, 'w'), indent=2)
```

If that fails, **stop and report** — that is a `PROMPT.md` stop condition.

---

## 3. The gates

`. ~/.pi/toolchains/env.sh` first. Then:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                     # 223 lib + 30 test results (10 in tests/worktree.rs, 3 in
                               # backend_contract.rs, 8 idle_stop.rs, 6 turn_timeout.rs, 14 delete.rs)
cd web && npm test             # 22 transcript tests (node's own runner)
cd web && npx tsc --noEmit     # not a gate phase, but a web edit that does not typecheck is a bug
scripts/verify.sh              # THE one-command gate since HS-006
```

`scripts/verify.sh` runs six phases, cheapest first: `fmt`, `clippy`, `test`, `build`
(`scripts/build.sh`, which itself runs `build-web.sh` — `npm test` then Vite — and `check-static.sh`
last), `embed` (`scripts/check-embed.sh`), then `serve` — the **shipped musl artifact** booted on
`--port 0` with a temp data dir, running **41 cases plus a CORS sweep (237 assertions)**, then torn down
with `SIGTERM` and an asserted exit 0. Three cases bring their own boot (`case_token_budget`,
`case_worktree`, and `case_docker_preflight`, which needs no server at all). It takes ~3 min (both musl targets) plus `case_idle_stop`'s real
minute. `--no-build` skips `build` and `embed`; `--keep` keeps the work dir.

A story is not done until all four are green on a clean tree, and then you run the story's own demo
yourself and paste its output.

**Run one build at a time.** A `scripts/build.sh` or `scripts/verify.sh` in the same tree while another
one runs can make `check-static.sh`'s banner wait flake under load (it happened once during HS-013: the
gate reported `FAIL build/failed` while the same command passed a minute later, alone). Serialise them.

**Rebuild after committing.** `git describe` is baked into the artifact and `check-static.sh` fails on a
stale `dist/`, so a commit without a rebuild leaves the gate red. Commit, then `scripts/verify.sh`, then
the demo you paste.

**Rebuild after editing `web/`, before any demo.** The artifact embeds `web/dist`, so a demo run against
an artifact built before the page changed is a demo of the *old* page — and it will look like your fix
did nothing. HS-020 lost a demo cycle exactly this way: the fix to `main.ts` was invisible until
`scripts/build.sh` ran again. `scripts/verify.sh` rebuilds, so "gate green, then demo" is the safe order.

---

## 4. The repository as it stands

```text
Cargo.toml                  workspace: member crates/holosmith, edition 2024, one release profile
Cargo.lock                  COMMITTED, registry-only. check-static.sh fails on a git+ source.
.cargo/config.toml          musl linkers + relocation-model=static (non-PIE) for both targets
pins/pi.txt                 THE ONLY copy of the pinned pi version. Never repeat the literal anywhere.
crates/holosmith/
  build.rs                  git tag (env → git describe → unknown), HOLOSMITH_TARGET, rerun-if-changed
  build_support/web_embed.rs  embed-vs-placeholder decision + placeholder marker
  migrations/0001_initial.sql   schema_version
  migrations/0002_sessions.sql  sessions + two indexes (schema 2)
  migrations/0003_events.sql    events + sessions.last_seq (schema 3)
  src/main.rs               dispatcher only
  src/cli.rs                clap: version, config {validate,show,edit}, serve [--port|--data-dir|--backend]
  src/commands/             version.rs, config.rs, serve.rs
  src/config/               mod.rs (typed Config, KeySpec walk, 26 keys), schema.rs, problem.rs,
                            render.rs, duration.rs, editor.rs
  src/backend/              mod.rs (Step, SessionBackend, Backends, BackendError, TurnEnd), stub.rs
                            (the scripted turn; `ignoring_abort`, `failing_at`, `failing_snapshot`),
                            docker.rs (refuses every operation until HS-025 fills them in)
  src/provisioner.rs        the four-step background task and its five session.status events; stops at
                            a step boundary when the row leaves `starting`
  src/turn.rs               the turn runner (HS-015) and the turn timeout (HS-022): touch → the
                            backend's turn → agent_settled + touch, abort_completed (or a `debug` line if
                            the ladder got there first), or unexpected_exit with the reason. Under a
                            deadline the ladder is *spawned* and the turn keeps being awaited;
                            `TURN_TIMEOUT`, `turn::timeout(minutes)`. `TurnEnd` is the backend's answer
  src/ladder.rs             the abort ladder (HS-016): clear_queue → abort → wait out abort_grace →
                            `running → stopped` with the caller's reason → stop. `climb` adds the
                            resume (the abort); `end_turn` returns `Ending::{Asked,Forced}` and does
                            not (the delete, the timeout and the budget). `FORCED_ABORT`
  src/sweeper.rs            the idle stop's task (HS-021): `Sweeper::sweep` (one pass, public for tests),
                            `run` (the tick loop), `spawn`; `IDLE_STOP`, `TICK` (5 s, a constant)
  src/deleter.rs            the delete flow's task (HS-019): revoke_key (a no-op until HS-039) →
                            `backend.delete` → remove the row *and its event log* in one
                            transaction → close the session's streams. `DELETED` is the rung-2 word
  src/runtime.rs            runtime::Backend {Stub,Docker} — which backend this run uses
  src/http/                 mod.rs, app.rs (Routes/App seam, AppState + provisioner()/turner()/
                            transitioner()/ladder()/deleter(), edge layer, is_guard_exempt), error.rs
                            (the one envelope, every code, ApiError::not_in_state / bad_prompt /
                            bad_steer / unsaved_work / unknown_session / cap_without_idle),
                            health.rs, guard.rs (the four checks), sessions.rs (POST+GET
                            /api/sessions, GET /api/sessions/{id}, POST …/prompt, …/abort, …/steer,
                            …/resume, DELETE /api/sessions/{id}[?force=true], TURN_STEER, EVICTED,
                            `Slot` + `slot_for` + `evict_one`, `apply_trigger`), streams.rs (the SSE
                            handler)
  src/sessions/             mod.rs (Status, Session, NewSession, RepoError), repository.rs (every
                            `sessions` statement + its SQL-location guard; includes touch,
                            `oldest_idle`, and `remove` — the row and its log in one transaction),
                            state.rs (the transition table, Transitioner::apply / apply_logged, the
                            set_status guard), activity.rs (the one `touch`: lock, warn, carry on)
  src/events/               mod.rs (Event, NewEvent, EventWriter, EventStream), writer.rs, repository.rs
                            (`insert_batch` drops an event whose session has gone; `delete_session_log`),
                            stream.rs (replay→live, lag), registry.rs (open-stream counts)
  src/worktree.rs           the session's worktree (HS-025, ADR-0029): `Worktree::of` (the naming rule),
                            `ensure` (create or adopt, answers `Ensured { base_commit, created }`), `ahead`
                            (`rev-list --count`), `remove` (registered? → remove → branch −D → prune), and
                            `GitError`. Sync and std-only; the backend calls it on `spawn_blocking`
  src/preflight.rs          the three startup checks and their sentences (HS-024, ADR-0028):
                            `check`/`check_with` (the `Tools` seam is the test's shim for git/df),
                            `Complaint { check, sentence }`, `socket_sentence`
  src/db.rs                 pragmas + read-back, apply_pending, Database::sessions()/events()
  src/sql.rs                the one SQL statement scanner + the shared source walk (cfg(test) guard)
  src/schema.rs             SCHEMA_VERSION (= 3), include_dir! embed, check_embed, contract_violations
  src/{serve,web,logging,clock,version,pins}.rs
  tests/                    version_cli, config_cli, http_contract, serve_cli, db_migrations,
                            pin_single_source, guards, session_repository, sessions_api, events,
                            streams, provisioning, state_machine, session_list, session_screen,
                            turns, abort, steer, resume, delete, eviction, idle_stop, turn_timeout,
                            token_budget, preflight, backend_contract, worktree, support/mod.rs
                            (the shared harness, which also has `repository`/`git`/`head`/
                            `branch_exists`/`worktrees` helpers for a real git repo in a `TempDir`)
web/                        Vite 7 + TypeScript, two screens (`/` and `/sessions/<id>`); untouched by HS-025
                            package-lock COMMITTED; main.ts is the DOM, transcript.ts the renderer's
                            rules (frame parsing, block model, affordances, `stoppedNotice` /
                            `stoppedDetail`)
web/test/transcript.test.ts 21 tests over synthetic frames, node's own runner (`npm test`)
scripts/build-web.sh        npm ci + npm test + npm run build, asserts web/dist/index.html
scripts/build.sh            web first, HOLOSMITH_REQUIRE_WEB=1, both musl targets → dist/<triple>/,
                            atomic copy, then check-static.sh. Uses `cargo build --locked`.
scripts/check-static.sh     file/readelf, empty-root serve + cmp against web/dist/index.html, marker
                            gate, app.db assertions, byte grep for the schema SQL, Cargo.lock guard
scripts/check-embed.sh      the three embed cases (present / absent warns / required fails)
scripts/verify.sh           the one-command gate (see §3)
scripts/import-linear.py    the MCP plumbing /tmp/lin.py reuses
docs/adr/0001…0029          read the recent ones before planning anything
docs/build.md               prerequisites, gates, verify.sh, guards, migrations, placeholders
docs/review/PER-33…PER-57   each a verbatim review + a `## Resolution` table
docs/stories/               the 80 story slices — the dependency order and definition of done
GLOSSARY.md                 the domain vocabulary. Check it BEFORE inventing a term.
```

### What exists and what does not

- **Working:** `version` (text + `--json`), `config {validate,show,edit}`, `serve` (control listener,
  embedded app, `/api/health`, `POST`/`GET /api/sessions`, `GET /api/sessions/{id}`,
  `POST /api/sessions/{id}/{prompt,abort,steer,resume}`, `DELETE /api/sessions/{id}[?force=true]`,
  `GET /api/sessions/{id}/events`), SQLite with three migrations, the guards, the event writer, the SSE
  stream, the stub backend and its four-step provisioner, the enforced state machine, the abort ladder,
  the delete flow, the cap with eviction, the Sessions screen with its create form and the
  "nothing was deleted" notice, the session screen at `/sessions/<id>` with a live transcript and a
  four-action composer.
- **Not built yet:** anything Docker beyond the preflight and the **worktree step** (HS-026 … HS-046 — the
  network, container and agent steps still refuse, each naming its story), the drain (HS-044), tool-output files and `output_ref` (HS-033), the **usage totals on the
  screen** (HS-034 — the columns are now real, the screen only says "no usage reported" when it must), the
  preview listener (HS-041), the stopped-session expiry (HS-064), `doctor` (HS-045), merge, publish, the hub,
  releases, and any **delete affordance in `web/`** (the API has the endpoint; the page has no button).
- **Four policies can now stop something and they must not be confused:** the cap evicts (`evicted`,
  inside a create), the sweeper stops a quiet one (`idle_stop`, on its own tick), the turn timeout ends a
  *turn* (`turn_timeout`, from the runner) and the token budget ends a *turn* (`budget_exceeded`, also from
  the runner, on one `fired` shared with the timeout so only one climb happens). All of them go through
  `Transitioner::apply`, and the row moves before the container in every one of them.
- **`runtime::Backend`** has `Stub` and `Docker`; `serve --backend` and the config key `backend` select
  it. `DockerBackend::new(projects_root)` **provisions the worktree and refuses every other operation**
  (HS-026 … HS-031 fill them in), and the default backend is `docker`
  (ADR-0001: the stub is opt-in), so a default build provisions a session to `error` with a reason
  rather than hanging in `starting`. That refusal is also the demo trick HS-019 used to provoke a real
  failed snapshot from the shipped artifact. `scripts/verify.sh` boots with `backend = "stub"` in its
  config, and so does `scripts/check-static.sh`'s empty-root run — a Docker run now refuses at startup on a
  machine that fails the preflight (HS-024), and that sandbox has no daemon, no `git` and no `df`.
- **`AppState`** is `{ config, runtime, database: Arc<std::sync::Mutex<Database>>, events: EventWriter,
  streams: StreamRegistry, backend: Backends }`, with **five** wiring helpers — `provisioner()`,
  `turner()`, `transitioner()`, `ladder()`, `deleter()`. The mutex is an explicit stopgap
  (ADR-0012/0013) — see §6.
- **`sessions::state::Transitioner::apply`** is the only way a session's state changes; `apply_logged`
  is the runners' wrapper (log, never return). A provisioning step report is `from == to` with `step`
  set and is *not* a table row.
- **The create is one lock scope plus a loop.** `create_locked` answers `Slot::{Inserted, Existing,
  Full}`; a `Full` makes `slot_for` evict one idle session (through `apply`, which takes the lock
  itself) and try again. Idempotency is checked first, so a retry never evicts anyone. `evict_one`
  moves the row **before** it spawns the backend's `stop`, because the row is what frees the slot.
- **`config`** already has `abort_grace` (default `10s`, **used** by the ladder), `idle_stop_min`,
  `stopped_ttl_days`, `min_free_gb`, `turn_timeout_min`, `max_session_tokens` and the rest — check
  `config show` before adding a key. The delete flow has **no** config key of its own, and `max_live_sessions`
  (default 3, ≥ 1) is the cap.
- **`POST /resume`** takes the transition, then spawns the provisioner, and **touches `last_active_at`**
  (a resumed session must not be the eviction's first candidate — which is now a real policy, not a
  future one). The cap is checked **before** the transition, and the resume answers `409 cap_reached`
  rather than evicting (ADR-0022, ADR-0024). `SessionBackend::resume` is still uncalled.

---

## 5. The per-issue cycle (what actually worked, twenty-four times)

Everything is **sequential: one writer at a time**, never two children editing files.

1. **Pick** the issue (ranking in §1) and read its Linear body plus the matching `docs/stories/` slice.
   Note the HEAD SHA — that is the review's fixed point.
2. **Plan.** For **L** stories, use a fresh `delegate` child briefed to run the grilling protocol
   (`~/.agents/skills/grilling/SKILL.md` + `domain-modeling`), which puts **one** round of frontier
   questions to *you* via `contact_supervisor`; answer with `subagent_supervisor({action:"reply", …})`.
   For **S/M** stories, write the plan yourself — it is faster and the analysis is usually already done.
   In either case: state the environment facts, name the files to read, list the decisions already
   settled, and **pre-answer the obvious round-2 questions**. A planner may write only `GLOSSARY.md`
   and a new `docs/adr/*.md`.
3. **Record in Linear before any code exists**: `save_comment` with the plan shape, a
   decisions→precedent table, acceptance restated as testable checks, non-goals, docs touched, the
   demo, and follow-ups; then set the issue `In Progress`.
4. **Implement.** Two patterns, both proven:
   - **S/M with the analysis in hand** (HS-011, HS-012, HS-014, and every story since HS-016): implement
     in-session.
   - **M/L with a lot of new surface** (HS-013's screen, HS-015's turn): hand a fresh `worker` child a
     *written* brief — the plan plus the "how", the ground rules (no commit, no Linear, no new
     dependency, `env.sh` first, don't edit `docs/stories/**`), the exact gate commands, and "budget
     discipline: keep the tree compiling; if you run long, stop green and report which steps remain".
     Launch it async; native completion wakes you. Twice a child found a genuine contradiction in my
     brief — expect that, and verify its claims rather than trusting them.
5. **Verify yourself.** Never trust a child's pasted output: run the gates and the demo. Do this every
   time; it has caught something each story (a wrong test count, a demo-revealed message bug, a panic
   path, and in HS-020 a demo assertion that passed over an already-stopped row).
6. **Review** with a fresh `reviewer` child running `~/.agents/skills/code-review/SKILL.md`, read-only,
   against `<fixed-point>...HEAD` plus untracked files, with the spec axis sourced from the Linear body
   + story slice + spec items. `git add -A` first, then `git diff --cached <fp> > /tmp/holosmith/reviews/
   PER-<n>-diff.txt` so new files appear. **The reviewer child has no shell or write tool**: its claims
   must be checked, and *you* save the review verbatim to `docs/review/PER-<n>.md`. Give it the plan and
   the ADR path too, so its findings are about what those documents do *not* already own.
7. **Fix** in the same session (or with a fresh `worker`), triaging every finding as fixed or
   dismissed-with-citation ("the reviewer misread the helper at …"). Re-run the gates. Append a
   `## Resolution` table to the review file and commit it with the fixes.
8. **Ship**: focused conventional commits naming the issue, the last ending `Closes <IDENT>`. Rebuild
   after committing (and after any `web/` edit — §3). Then `save_comment` with what shipped, the gates,
   verbatim demo output, acceptance item by item, and follow-ups; set the issue `Done`; re-read it to
   confirm.
9. **Refresh this file** at a natural stopping point (counts, HEAD, next issue, the next story's facts).
   It is a cold-start snapshot. The operator asked for it once at the end of a session; refreshing it
   after each story is cheap insurance if the session dies.

Commit messages are prose about *why*, not a restatement of the diff. Look at the last twenty-two for tone.

Working artifacts (`/tmp` is ephemeral; the durable record is Linear + `docs/review/` + `git log`):

```text
/tmp/holosmith/plans/PER-<n>.md           the plan
/tmp/holosmith/plans/PER-<n>-linear.md    the Linear comment body
/tmp/holosmith/plans/PER-<n>-impl-brief.md
/tmp/holosmith/plans/PER-<n>-ship.md      the closing Linear comment body
/tmp/holosmith/reviews/PER-<n>-diff.txt   the diff artifact for the reviewer
/tmp/holosmith/reviews/PER-<n>-review-brief.md
/tmp/holosmith/reviews/PER-<n>-review-verbatim.md
/tmp/holosmith/demo-per-<n>.sh|.py        the demo, with its client half
/tmp/holosmith/web-check*.mjs             the Chromium half, when the story has a screen
```

---

## 6. Landmines (each of these cost real time)

### Build and toolchain

- **The pinned pi version lives in exactly one tracked file, `pins/pi.txt`.** `tests/pin_single_source.rs`
  fails if the literal appears in a second tracked file — including `docs/review/*.md` and this file. A
  commit message is fine; a doc file is not. Every saved review and Resolution table deliberately never
  spells it out.
- **Never export `RUSTFLAGS`.** It silently disables the per-target `rustflags` in `.cargo/config.toml`
  and ships static-PIE. `check-static.sh` asserts the resulting property so a mistake fails loudly.
- **`scripts/build.sh` builds with `cargo build --release --locked`.** Adding a dependency without
  regenerating `Cargo.lock` fails the whole gate. Regenerate, check the lock diff shows only the edge you
  meant, then run the gate.
- **A bare `cargo build` without `web/dist` succeeds with a placeholder page.** `build.sh` sets
  `HOLOSMITH_REQUIRE_WEB=1` and `check-static.sh` greps the artifact for the marker.
- **The artifact embeds `web/dist`, so a `web/` edit needs a rebuild before any demo.** `verify.sh`
  rebuilds; a hand-run demo does not. See §3.
- **No `awk`, no `shellcheck`.** Scripts must use `sed`/`grep`/`tr`/`cut`/`cmp`/`find`/`timeout`. A
  browser exists now, but nothing in `scripts/` may require one.
- **One build at a time**, and **rebuild after every commit** (see §3).
- **node's test runner needs node 22.18+** (native type stripping); this machine has 24.

### Tests

- **Test isolation.** Every test that starts `serve` must give it a temp `data_root` inside its own temp
  dir: this sandbox lets an unprivileged user write `/var/lib`, so a test once created and migrated the
  *real* `/var/lib/holosmith` and still passed.
- **Two `Database::open`s on one path in one process is ADR-0009's measured "database is locked" trap.**
  A shared harness must open exactly one (`OnceLock`); a per-test harness always takes its own
  `TempDir` (`tests/support/mod.rs` carries the pid in the directory name).
- **Same-process SQLite contention bypasses the busy handler.** A two-thread concurrency test is flaky by
  construction; test the lock with a second *process*.
- **A guard test that shares a scratch directory with another guard flakes.** Guards that walk `src/**`
  share `sql::guard`; a directory name comes from its own table word or label.
- **A rule that walks `src/**` must prove it found something.** HS-012's `set_status` guard asserts the
  walk saw both allowed files *before* asserting no offenders — otherwise a broken walk passes green.
- **A startup refusal changes what every default-backend boot does** (HS-024): seven `tests/serve_cli.rs`
  tests and `check-static.sh`'s empty-root run failed the moment the Docker preflight landed. The honest
  fixes are `backend = "stub"` in a shared test config, a fake daemon socket for the two tests that must
  boot Docker, and `--backend stub` in the empty-root run — never a flag that skips the check.
- **A test that asserts only a status is how a wrong code ships.** Assert status **and** `code` **and**
  `content-type`, and prove a new guard fires by removing the behaviour and watching the test go red.
- **The harness's stub is deliberately slow** (`SLOW_STUB_STEP` = 60 s per step) so no test that is not
  about provisioning loses the race against HS-011's background task. Tests that need provisioning to
  finish use `support::with_stub(state, support::fast_stub())` (1 ms). Waiting for a row to reach `idle`
  is `wait_until(...)`, never a `sleep`. That slow stub is also the only way to hold a session in
  `starting` on purpose (HS-020's cap refusal).
- **A row can be ahead of its event.** The runner writes first and the writer commits every 25 ms, so
  "the row says `idle`" does not mean the event is committed. Tests read the log by event count, and the
  stream tests take N frames. HS-018 hit the sharper form: a `prefix` assertion over a log captured from
  a row that had just changed was **vacuously true over zero events** — wait for the event you mean, then
  assert the log is not empty. HS-019 hit it again from the other side: a *before* count taken right
  after `wait_status(idle)` was zero, so "the removal took the log too" would have passed for the wrong
  reason.
- **A background call is not there when the answer is.** HS-020's eviction spawns the backend's `stop`,
  so a test that asserts the backend call must `wait_until` on the recorded call — the **row** is what
  the 202 promised, not the container call.
- **`last_active_at` is whole seconds, so "it moved" is usually the next touch, not yours.** HS-017's
  review proved this by mutation: a steer's `touch` could be deleted and the test stayed green, because
  the turn settled a second later. Push the column into the past first
  (`sessions().touch(id, now - 60)`), then act, then read **immediately** — a `wait_until` reintroduces
  the race. HS-018 changed its stop helper to take a `reason` for the same class of reason: a `None`
  reason made "the resume clears the reason" untestable. HS-020 ages rows the same way to make "least
  recently active" a fact, and ages **two** rows to the same second to prove the `id ASC` tie-break.
- **A prompt turns a race into an expected outcome.** Two prompts can both see `idle`; the loser gets
  `StateError::Illegal` and `apply_prompt` retries once when the row is idle again. Do not "fix" that by
  holding the lock across the read and the apply.
- **A test's claim can be half-strength and still green.** HS-020's review caught one: "rules out the
  lowest id" over **random** ids is a coin toss. If a test claims to rule out a competing rule, make the
  competing rule's answer *deterministic* and wrong — or split it into two tests (which is what the fix
  did). Mutating the rule and watching the test fail is the only proof.

### HTTP and the envelope

- **Every fallible axum extractor answers `text/plain`, not the envelope** — `axum::Json<T>`'s
  rejection, `Bytes`'s (`UnknownBodyError` 400, `LengthLimitError` 413) and `Path<String>`'s (a `%FF`
  segment is a 400). Take the body and the path as `Result<…, Rejection>` and map them yourself.
  `RawQuery` is the exception: its rejection is `Infallible`, so a handler can take it directly.
- **The `Routes`/`App` seam is load-bearing**: `Routes` has no constructor from a bare `Router` and no
  accessor yielding one, `axum::serve` appears once, and `Router::layer` runs in exactly one place —
  `App::new`. `tests/guards.rs` pins the one `.layer(` and the manifest's read-only/mutating kinds.
- **A `GET` route cannot be mutating.** Use `Routes::get`/`head`/`post`/`delete`/…; the manifest records
  the kind. `DELETE` is the only non-`GET`/`POST` verb in the table so far (HS-019's route).
- **Errors are one envelope**, `{"code","message"}` (ADR-0008); codes are constants in `http/error.rs`.
  No test pins a message sentence — but a message must describe *its own* request (HS-015's prompt
  reused the create's sentence until a demo showed it; HS-019's `GET` after a delete said "no such
  endpoint" until a demo showed that too). Two requests may share a code and differ in sentence
  (`cap_reached` for the resume, `cap_without_idle` for the create).
- **`is_guard_exempt` is the only exemption list** (true only for `GET`/`HEAD /api/health`).
- **A mutating request needs `Content-Type: application/json` even with no body.** The `bad_client`
  guard reads the content type as well as `X-Holosmith-Client`, so `POST /abort`, `POST /resume` and
  `DELETE` must send it — a client (or a test) that omits it gets `403 bad_client` and thinks its
  endpoint is missing. The guards check the origin **before** the client header, so a test that omits
  both proves `bad_origin`.
- **In `scripts/verify.sh`, a stream needle can be answered by the *replay of an earlier turn*.** Every
  turn's settle payload is identical, so "wait for `running → idle`" returns the previous turn's frame.
  Use a needle unique to the action (`"reason":"abort_completed"`, `"reason":"evicted"`, the steer's
  text) or poll the row (`wait_for_idle`, `wait_for_gone`), which is what the steer and delete cases do.
- **A `case_*` must own the state it asserts.** HS-020's cap case used to require a session created two
  cases earlier to still be provisioning — a bet on the host's speed — and was moved last, where it
  creates the session it depends on. Before adding a case, ask what it leaves behind *and* what it
  needs from what came before.
- **Banner line order is pinned by tests** (`log`, version line, `config`, `data_root`, `control_listen`,
  `control_origin`, `preview_listen`, `database … schema n`) — and `tests/serve_cli.rs` asserts stderr has
  **exactly eight lines** at default filter. A new `info!` at startup therefore fails a gate test; HS-021
  and HS-022's startup facts are `debug!` for that reason. A *per-event* `info!` line is fine (the gate
  greps for it) because it happens after the banner and only when the thing happens.
- **A `select!` branch that awaits a ladder climb is a bug.** `select!` drops the loser, and a climb
  dropped after rung 2 moved the row leaves the session `stopped` with nobody to resume it — silently,
  because the runner's own ending is the documented `debug` line. `Ladder::spawn` is the entry point
  (HS-022's review caught this).
- **`fetch_sse_until`'s line budget is 800, not 200**, because the shared session's log grows with every
  turn this run takes (four or five turns is ~300 lines of replay). A story that adds turns should re-check
  it; a needle that a *previous* turn's frame can satisfy is still the more common trap (see the stream
  rule above).
- **`--port 0` re-derives `control_origin`** from the bound port after `bind`; the guard compares the
  browser's `Origin` as text, which is what makes it work in tests and demos.
- **`{id}` is axum 0.8's capture syntax**; the spec writes `:id`.

### Data and state

- **A seq counter must never be derived from `MAX(seq)`.** Compaction (HS-060) deletes rows, so the
  counter is the durable `sessions.last_seq`, bumped in the same transaction as the insert.
- **The event writer has one broadcast channel for all sessions**, so a per-session consumer filters by
  `session_id` before comparing seqs — otherwise a foreign seq corrupts the resume point (HS-010's
  blocker).
- **Broadcast strictly after `COMMIT`**, and prove it with a second connection reading inside the
  subscriber; counting rows is vacuous.
- **`id TEXT PRIMARY KEY` is nullable in SQLite.** The invariant is the repository's, not the schema's.
- **`SCHEMA_VERSION` is a literal**; `check_embed` fails the build if it disagrees with the embedded
  files. Bump it in the same commit as the migration. `ALTER TABLE … ADD COLUMN` goes **last**.
- **The repository owns every statement against its table**, enforced by a walk over `src/**`. A new
  *reader* goes through `select()`/`one()`, never a hand-copied column list — HS-020's review caught one
  that had re-spelled all fifteen columns, which would have missed a column a migration added.
- **No state changes outside `sessions::state::Transitioner::apply`.** `set_status` may be *named* only
  in `sessions/repository.rs` and `sessions/state.rs`; a walk over `src/**` enforces it. Integration
  tests outside the crate may still call the raw write (they test the repository).
- **The `session.status` payload is `{from, to, step, reason}`**, four keys, explicit `null`s. A step
  report is `from == to` with `step` set and writes nothing; a transition carries `step: null`.
- **The create's initial state is the table's `none` row**, not a second spelling of `starting`.
- **A backend can emit an event but cannot write the row.** Its `EventWriter` takes free-form
  `kind`/`payload`, so a *forged* `session.status` frame is possible and is exactly the lie ADR-0016
  forbids. ADR-0019 records it as a known hole; closing it means typing the payload (no story yet).
- **A delete takes its row's event log with it** (`sessions::repository::remove`, one transaction), and
  the writer **drops** an event whose session has gone instead of refusing the batch (HS-019). Two
  consequences a test must respect: a deleted session's events cannot be asserted *after* the removal
  (assert the before-count while the row still exists, as `tests/delete.rs` does), and the `deleting`
  event is usually dropped rather than delivered because the removal finishes inside the writer's 25 ms
  tick — so never assert that a watcher saw it.
- **A create evicts outside its own lock scope.** The insert is one scope (`create_locked` →
  `Slot::{Inserted,Existing,Full}`) and an eviction is `Transitioner::apply`'s, which takes the same
  lock; the loop in `slot_for` joins them and terminates because each eviction strictly lowers the live
  count. Do not "simplify" that by writing the status inside the scope.

### Web

- **No new npm dependency, no framework, no router.** `EventSource`, `crypto`, `fetch` are the platform;
  `package-lock.json` is committed; `npm test` is node's own runner.
- **`web/src/main.ts` keeps no state beyond the DOM** (plus the create's pending Idempotency-Key). The
  transcript's rules live in `web/src/transcript.ts` and are tested without a browser — including
  `stoppedNotice`/`stoppedDetail`, because a *sentence* is a rule too.
- **A row's text is written in two places, and they must be one function.** A row is drawn once
  (`sessionRow`) and patched per frame (`applyStatus`); HS-020's blocker was a sentence written only in
  the first. Anything the frame path must show belongs in a pure `transcript.ts` function both call.
  `stoppedNotice` is now **two words wide** (`evicted` "to make room", `idle_stop` "sitting idle"), each
  with its own test and neither shared with `forced_abort`, `deleted` or `turn_timeout` — a stopped row
  for those keeps the dash, and `turn_timeout` is resumed at once anyway so it shows as `idle ·
  turn_timeout`. Adding a third word means a third sentence *and* a test that it is not another's.
- **A delta is one appended text node; a snapshot is `replaceChildren`.** Never `textContent +=` (O(n)
  per delta ⇒ O(n²) per turn), never a document query per frame — blocks are a `Map<message id, nodes>`,
  tools a `Map<tool id, nodes>`.
- **Never `innerHTML` with server data.** Every node is `createElement` + `textContent`.
- **A form inside a page must swallow its own submit** — with no `action` it is a GET of the current path
  and a full reload, which discards the transcript (HS-014's review caught exactly that).
- **The session screen keeps its stream open in every state**, unlike a list row (ADR-0017 closes a dead
  row's source). Both are deliberate; ADR-0018 says why. A delete's stream is closed by the *server*
  (`StreamRegistry::close`), so a row for a deleted session ends and a reconnect is `404`.
- **`turn.steer` is the one frame with no message id** (ADR-0021): its block is created per frame, and
  `order()`/`toolOrder()` do not see it. Do not "fix" that by minting an id without reading ADR-0021's
  price — the log is durable, so two shapes of one kind would live in it forever.
- **The composer is four actions in three states**: prompt on `idle`, steer+abort on `running`, resume on
  `stopped`/`error`, nothing on `starting`/`deleting`. Its POST helper resumes **once** and retries
  **once**, and `abort` passes `mayResume: false` — a resume ends `idle` and abort needs `running`, so a
  retried abort can only fail (HS-018's review found that).
- **The stub's ids carry the process instant** (`msg-<nanos>-<n>`), because a restart that started over
  at `msg-1` would fold a new turn into a previous turn's block against a durable log.
- **`crypto.randomUUID` needs a secure context**; the create form falls back to one `getRandomValues` v4
  and shows the reason if neither exists.
- **A page cannot set `Last-Event-ID`.** `EventSource` resends the last SSE `id` it saw.
- **`case_app_shell` compares `GET /` byte-for-byte with `web/dist/index.html`**, and
  `case_session_screen` does the same for `/sessions/<id>`, so a stale `dist/` fails the gate.

### Config and docs

- **Config output** uses `# <key> unset` rather than omitting an unset optional, and `config show` output
  must re-validate clean. `tests/golden/config-default.toml` pins the exact output, so **a new config
  key means updating the golden file and `KNOWN_KEYS.len()`** (currently 26).
- **`docs/build.md` is a story deliverable, not orchestrator-only.** If a story changes prerequisites or
  gates, the doc is in scope, and its prose must be reflowed (~100 columns), not just appended to.
- **Every story adds at least one `case_*` to `scripts/verify.sh`.** The rule is in bold in
  `docs/build.md`, and the reviewer enforces it. The current gate is 36 cases / 169 assertions.
- **Adding an ADR means updating `docs/stories/README.md`**: the "Decisions taken while slicing" count
  (now **twenty-six**) and the bullet list. A reviewer will check the links resolve — three did not,
  once.
- **An ADR that a later story changes gets a one-line "Superseded by ADR-00xx" note**, not a rewrite
  (ADR-0012 and ADR-0013 do this; HS-015 added two to ADR-0018).
- **`GLOSSARY.md` is the vocabulary authority.** Check it before inventing a term. Recent entries to
  respect: **Step**, **Backend**, **Trigger**, **Transition**, **Sessions screen**, **Session screen**,
  **Composer**, **Transcript**, **Delta kind**, **Message id**, **Turn boundary**, **Turn runner**,
  **Tool call**, **Usage**, **Evict**, **Cap**, **Delete flow**, **Unsaved work**. `Status` in code is
  **State** in the glossary; a delta's node is a "kind's node", never a "channel"; a second spelling is
  forbidden, including inside an entry's own prose.
- **`RESUME.md` is the one place this file's own claims can rot.** It contains counts, SHAs and "next
  issue" statements; anything that changes them is a reason to edit it in the same commit.

### Landmines this session added (HS-023, HS-024)

- **A startup check must run before the thing it protects.** HS-024's first draft put the preflight *after*
  `Database::open`, so a refused Docker start had already created and migrated `app.db` while the comment and
  the ADR said "no migration" — a reviewer's P1, and a scripted edit of mine that put the block in the wrong
  place. The gate case now asserts that no `app.db` appeared. **After any scripted move of a block, read the
  resulting order, not just the diff.**
- **A new startup refusal breaks every test and script that boots the default backend.** Seven
  `tests/serve_cli.rs` tests and `scripts/check-static.sh`'s empty-root run failed the moment the preflight
  landed, because the default backend is `docker` and this machine has no daemon. The fixes are the honest
  ones: the shared test config says `backend = "stub"` (with the reason in its doc comment), the two tests
  that must boot a Docker run bring a **fake daemon socket** (one helper), and `check-static.sh` passes
  `--backend stub` because that run is about linking.
- **A check that needs a machine cannot be tested against the machine.** `preflight.rs` takes a `Tools { git,
  df }` seam so a test points it at shims it wrote. THE ALTERNATIVE IS A TRAP: setting `PATH` in a test is
  `unsafe` in edition 2024 (and this crate `deny`s `unsafe`), and it is shared state a concurrent test sees.
- **The writer's 25 ms tick is a race in any test that reads a row right after a turn.** HS-023's own
  `two_turns_add_up` flaked 2 runs in 5 by reading `usage_in` immediately; `wait_for_usage` waits for the
  value. This is the same lesson as HS-017's `last_active_at` and it keeps being relearned.
- **A turn whose last frame is its `message.end` can settle between two polls.** A policy polled at 100 ms
  cannot catch it, so the runner records the reason on the ending it wrote (HS-022's settle-after-the-deadline
  shape) and looks once more one writer tick later — because a tally still in the writer's queue is not a
  tally of nothing.
- **`awk` is still absent** and a `command -v awk` guard caught me using it in a shell one-liner. Use
  `git`/`grep`/`sed`/`cut`/`while read`.

### Landmines HS-025 added

- **`git worktree add` resolves a *relative* path against the repository**, not the caller's directory
  (measured, git 2.43): `git -C repo worktree add ../x` lands inside the repository. Every path in
  `src/worktree.rs` is absolute, and `projects_root` is validated absolute.
- **`git worktree list --porcelain` prints the *resolved* path and does NOT quote it** — a projects root
  reached through a symlink compares equal only after `canonicalize`, and one with a space or non-ASCII
  bytes is printed raw (measured; `core.quotepath` does not change `--porcelain`). `tests/worktree.rs`'s
  adoption test uses a temp dir with a space in its name so this cannot regress.
- **"Already gone" is an exit code, not an exception**: `git worktree remove` on an unregistered path exits
  128 and `git branch -D` of a missing branch exits 1. `remove()` asks `registered()`/`branch_exists()`
  *before* running either, instead of reading git's English.
- **A teardown must succeed when the project repository is missing too** (a delete keeps a row `deleting`
  on a backend failure, ADR-0023). A *directory* that exists but is not registered is left alone — HS-044's.
- **The provisioner only undoes what a provision made.** `Provisioned::undoable` is the rule: an adopted
  worktree's commits survive a later step's failure. Mutating it back to "always undo" makes
  `a_later_step_failure_does_not_take_an_adopted_worktree_back` go red — that test is the guard.
- **`case_worktree` needs a Docker boot on a machine with no daemon.** It answers the preflight's `_ping`
  from a ten-line `python3` unix socket (`fake_docker_ping`), and its python3 guard must stay **above** its
  `stop_serve`: a skipped case that stopped the server first leaves `phase_serve`'s own `stop_serve` with an
  empty pid, and `kill -TERM ""` exits 1 → the gate fails with a message about SIGTERM.
- **`tests/preflight.rs` flakes about 3 runs in 40** with `git is not runnable (Text file busy (os error
  26))`: its shims are written and exec'd while other test threads fork. Serial runs are clean (0/12).
  Writing the shim aside and renaming it did **not** help (measured) — the forked child inherits the write
  descriptor of the inode that is then exec'd. Re-run the binary; the real fix is to write the shim from
  another process or to build every shim once under a lock. **Not fixed yet, and it can redden the gate.**

### Reviews

- **Reviews are saved verbatim** and a `## Resolution` section is appended afterwards. Do not edit the
  review body; do not delete a dismissal.
- **The reviewer has no shell.** Its claims must be checked: HS-014 and HS-015 each had one finding the
  reviewer got right and one it got wrong (it read a helper as the wrong function; it called an
  `unreachable!()` path unreachable when a race reached it). HS-019's review read a *stale diff artifact*
  and reported a contradiction that had already been fixed — **regenerate the artifact after any doc edit
  you make while the reviewer is reading**, and say so in the Resolution if it happens.
- It reliably finds one real should-fix per story, and since HS-019 usually one **P1-class** one: HS-019's
  was a refusal that had already ended the turn, HS-020's a sentence written on only one of two DOM
  paths. Both were found in the *acceptance bullet's* own words. Take them seriously.
- **Two of the last four reviews came back `BLOCK`, and each time it was the reviewer, not the plan,
  that was right** — HS-022's was a `select!` the plan itself had argued for, HS-024's a startup check
  ordered *after* the thing it protects. **HS-022 came back `BLOCK`, with a P1 on each axis, and both were
  real.** The spec one was a
  concurrency bug the *plan and the ADR had argued for* (a `select!` around the ladder) — a good plan is
  not a defence. The standards one was a **scripted-edit bug of mine**: a Python insertion into
  `docs/stories/README.md` duplicated a bullet onto one line. After any scripted doc edit, `git diff` the
  file's changed lines and read them; `cargo`/`npm` would never catch it.
- **The reviewer's cheapest useful ask is usually a test that cannot fail.** Two stories running, it
  named one (`each_turn_gets_its_own_clock` built a fresh object per case; `the_bound_is_the_config_key_in_minutes`
  bracketed too loosely) and the fix was to make the wrong design fail. Do that, then mutate the
  behaviour and watch the test go red.
- **Mine its "verification" section.** Its "could not verify without running" list is exactly the
  checklist to run yourself, and its "verified by reading" table is what to quote in the ship comment.

### Demos

- **Demo commands must be bounded.** Background a server with every fd redirected and `timeout`, and kill
  it by its own pid. A backgrounded server keeps the tool call's stdout open and hangs the child.
  `pgrep -f 'holosmith serve'` matches unrelated processes.
- **Launch demos from a script file, not an inline `&` chain** — `cd … && … && binary serve &` puts
  the whole chain in a subshell, so `$!` is not the server's pid and `wait` reports the shell's status.
- **`scripts/verify.sh`'s `fetch` waits for EOF**, which an SSE stream never sends. Use `fetch_stream`
  or `fetch_sse_until`.
- **A demo assertion can pass vacuously.** HS-020's first Chromium check waited for "some row is
  stopped", which an *already stopped* row satisfied instantly, so it proved nothing — it was caught only
  because a stale artifact then made it fail. Key a wait on the id (or the value) you mean, exactly as a
  stream needle must be unique.
- **A `web/` demo needs a rebuild first**, or it is a demo of the previous page (§3).
- **A create's state is not stable in a demo**: the provisioner runs 200 ms/step under `--backend stub`,
  so a `GET /api/sessions` a second later shows `idle`, not `starting`. Assert what the answer *is*.
  That pace is also what makes a real 409 `unsaved_work` demonstrable from the shipped artifact: serve
  with the default `docker` backend, create (the session lands in `error`), and the delete's snapshot is
  refused.

---

## 7. Decisions taken on the operator's behalf (do not silently undo)

| Decision | Why | Where recorded |
| --- | --- | --- |
| Default branch stays `master` | the repo's own default; renaming would be an invented change | commit history, every run note |
| Ship locally, never fake a push | no credentials exist (`Permission denied (publickey)`, re-tested); reporting the blocker is the honest path | every issue's run note |
| Rust + root Cargo workspace, one binary crate | stories assume Rust | ADR-0004 |
| Static non-PIE musl on both arches, proven with bwrap + qemu, container runs deferred to HS-046 | no Docker daemon; ADR-0003 says a check belongs to the story where changing the design is still cheap | ADR-0005 |
| Config: one typed value, `KeySpec` single table, three-pass validation, `# <key> unset` | "reports every problem at once" is impossible with a single `toml` parse | ADR-0006 |
| axum + the `Routes`/`App` seam, placeholder+marker for the web app, shutdown deadline with streams cut | measured: `axum::serve` rejects a tower-wrapped service; an endless stream blocked shutdown 29 s | ADR-0007 |
| One JSON error envelope | the spec names codes, never bodies | ADR-0008 |
| SQLite: pragmas read back before any transaction, one transaction per start, SQLite's own write lock, `_contract_` naming, older-binary warns | measured: WAL inside a transaction is silently refused; only prepare-time errors carry SQL text | ADR-0009 |
| Guards are one layer inside `App::new`; CORS stripped, not just avoided; `GET` mutability prevented by the route vocabulary | "no CORS header, ever" cannot be a review checklist | ADR-0010 |
| Session id is 16 lowercase hex from `randomblob(8)`, generated by the repository before the insert | the id is pasted into a branch, a container name and two paths; hex is safe in all four and needs no crate | ADR-0011 |
| `status` has no `CHECK`; the six values are a Rust enum | a corrupt row should be a read-time error, and HS-012 adds transitions without rewriting a constraint SQLite cannot drop | ADR-0011 |
| `Arc<std::sync::Mutex<Database>>` in `AppState` as an explicit stopgap | `rusqlite::Connection` is `Send` but not `Sync`; ADR-0009 deferred the sharing decision | ADR-0012 |
| Validation is **shape, not vocabulary** (no model/thinking list) | the spec enumerates neither; inventing a list is a third vocabulary, and HS-031 confirms pi's real values | ADR-0012 |
| The cap refuses with 409 rather than evicting | nothing is evictable until HS-019 can stop and HS-020 can evict | ADR-0012 |
| The mutex stays and the writer task is added on top | ADR-0012's "HS-009 replaces it" conflated a lock with a task | ADR-0013 |
| The SSE heartbeat is axum's `KeepAlive`; `futures-core` is a direct dependency | writing our own comment line would double the heartbeat | ADR-0014 |
| One backend trait with **one method per provisioning step** plus `undo`; an enum `Backends`, not `dyn` | the story's progress reporting *is* step granularity; `async fn` traits are not object-safe | ADR-0015 |
| The stub's per-step delay and the turn's pace are one constructor argument (200 ms under `serve`) | a stub's pacing is not a machine setting | ADR-0015 |
| `backend` **is** a config key (default `docker`), with `--backend` overriding it | the story names both, and ADR-0006 says an override applies to the loaded value | ADR-0015 |
| The transition table is data keyed by `(from, trigger)`; a caller never names a target | a story cannot invent an illegal state, and the write and its event cannot diverge | ADR-0016 |
| "Impossible outside it" is a source walk over `src/**`, not a `pub(crate)` write | the repository's tests live outside the crate and are HS-007's coverage of the write | ADR-0016 |
| The recovery row admits the three **live** states, not `any` | the spec's cell is "server crash", not a state | ADR-0016 |
| `GET /api/sessions` answers `{"sessions":[…],"network_default":…}`; one `EventSource` per visible live session; the page holds only the DOM | one round trip for the form's default; the cap bounds open streams | ADR-0017 |
| The transcript's five event kinds are pinned before their producer; a block is keyed by message id; a delta is one appended text node and a snapshot the only replacement | HS-015 (and HS-032) must emit one shape, not choose their own; re-reading accumulated text per delta is O(n²) per turn | ADR-0018 |
| The renderer's rules live in `web/src/transcript.ts`, tested with node's own runner inside `build-web.sh` | no browser here and no producer at the time; node strips types natively, so it costs no dependency | ADR-0018 |
| The backend emits a turn's frames; the server owns every state change; the prompt's transition is the handler's | the 202 must promise `running`, and a backend must not be able to move a row | ADR-0019 |
| Refusals are `409 session_<state>`, the state's own word — not an umbrella code | the spec names two, HS-017 names a third, and a derived family has no second vocabulary | ADR-0019 |
| `message.end`, `tool.start`, `tool.end` payloads are pinned now; `output_ref` and usage persistence wait | HS-033/HS-034 own their full treatment, and the screen renders them minimally meanwhile | ADR-0019 |
| Story docs in `docs/stories/` are not edited — they are the source the Linear issues were imported from | editing them drifts from Linear; the ADR carries the contract instead | PER-34 plan comment |
| S/M stories are implemented in-session; M/L with new surface goes to a worker child with a written brief | the orchestrator already holds the plan for small ones; a fresh context is better for a large slice | §5, and every Linear note since HS-011 |
| A turn's end is typed (`TurnEnd`), and cancellation is the backend's — no server token, no `select!` (the `select!` half is amended by ADR-0026: one exists, but it cancels nothing and the ladder is spawned) | the agent process is what must stop, and a token would let the server believe it had stopped a turn still running | ADR-0020 |
| Abort climbs two rungs, the row moves before the container is killed, and the ladder's reason is a parameter | the other order leaves a row saying `idle` over a dead container; HS-022 passes `turn_timeout` and HS-023 `budget_exceeded` | ADR-0020 |
| A steer is `turn.steer` with `{"text"}` and **no** message id; the agent is asked before the record is written | a steer is one frame that nothing continues, so an id would have no second use; the record must claim only what was delivered | ADR-0021 |
| No stop endpoint; resume is the only way back, the cap is checked before its transition, and `SessionBackend::resume` stays uncalled | the spec has no stop endpoint, and the policies that stop a session are HS-020/021/044 | ADR-0022 |
| The delete's snapshot is the request's and the removal a task's; the turn is ended by `end_turn` (no resume); the event log goes with the row, so the writer drops an event whose session has gone; idempotency is keyed to the row's state; `force` is the literal `force=true` | only a synchronous snapshot can answer 409 and leave the row intact, and a log nothing can read is dead weight | ADR-0023 |
| The create evicts the least recently active `idle` session outside its own lock scope; only `idle` is a candidate; the row moves before the container and the `stop` is **spawned**; a create with nothing idle answers `cap_reached` with its own sentence while the resume keeps ADR-0022's refusal | an eviction is a state change, and a state change takes the lock; an answer must not wait for container work (spec item 7); the two requests have different next steps | ADR-0024 |
| The idle stop is a sweeper task on a **fixed five-second tick** (a constant, not a config key), its candidates are every `idle` row past `idle_stop_min` from one repository read, each stop is one `apply` plus one spawned best-effort `stop`, and `idle_stop_min = 0` spawns nothing | pacing is not a machine setting (ADR-0015) and the spec's config list is exhaustive (ADR-0006); a policy over rows needs no lock the create holds | ADR-0025 |
| The turn timeout is the **turn runner's own deadline**, it hands the ladder a task of its own rather than a `select!` branch, and both endings record `turn_timeout` | the row keeps no per-turn timestamp (`last_active_at` moves on a steer, so a watcher would let a steered turn run for ever), and a dropped climb leaves a session stopped with nobody to resume it | ADR-0026, amending ADR-0020 |
| The **tally is the event writer's** (inside the seq transaction, so the events repository parses exactly one kind's payload) and the **ceiling is the runner's** (a 100 ms poll, `>=`, first policy to fire owns the reason, a settle over the ceiling records the reason instead) | the backend emits `message.end` and the runner never sees the frame; a subscriber would add lag and a second parser; a policy that must record a reason owns the ending | ADR-0027 |
| The **Docker preflight is a startup refusal** with `Complaint` values reported all at once, `hub` and `stub` exempt, and no new dependency (`df`, `git --version`, a hand-written `_ping`) | the story says "fail at startup instead of mid-turn", item 10 §5 makes the same check `ExecStartPre`, and `unsafe` is denied so `statvfs` was not reachable | ADR-0028 |
| The **worktree's five host commands live in one `src/worktree.rs` value**, the backend runs them on `spawn_blocking`, and the **provisioner** writes the two columns the step reports | item 1 names them as host commands that four later stories read, and ADR-0019 forbids a backend writing a row | ADR-0029 |
| A worktree step that **adopted** a worktree is **not undone** when a later step fails; a delete still removes it | those commits are the agent's work, and deleting them to tidy up after a step that made nothing is data loss; a delete's session is going away and was snapshotted | ADR-0029 |
| **`ahead` is a function, not a column and not an endpoint**, and the story's demo is recorded as half-showable | no column exists, HS-061 owns `GET /changes`, a per-row git call in the list endpoint is a second data path; the step after the worktree refuses, so a live worktree cannot be shown from the artifact | ADR-0029 |

---

## 8. Resume checklist

```bash
cd /home/adamolsson/projects/holosmith
git status --short                          # must be clean
. ~/.pi/toolchains/env.sh
python3 /tmp/lin.py issues | grep -v ' Done ' | head   # next unfinished (recreate /tmp/lin.py, §2)
python3 /tmp/lin.py issue PER-58            # sanity-check Linear is reachable and scoped
scripts/verify.sh                           # baseline green (~6 min, 41 cases, 237 assertions)
```

Then start the cycle at **`PER-58` (HS-026 — worktree refusal rules)**. Read §1's facts first: the runner
owns both turn policies, the spend is a column the writer adds to, the worktree step is real and idempotent,
and the page has been rendered (so a DOM claim needs a Chromium run — and a `web/` edit needs a rebuild
before that run).

### What HS-026 is about, so the first brief is fast

Acceptance, verbatim from `docs/stories/phase-1b-docker-backend.md` (Size S, spec item 1):

- [ ] 422 `git_not_directory` when the project's `.git` is not a directory.
- [ ] 422 `submodules_present` when submodules exist (their hooks live under `.git/modules`).
- [ ] 422 `worktree_config_enabled` when `extensions.worktreeConfig` is on.
- [ ] Checks run before `git worktree add`, so a refused session leaves nothing behind.
- [ ] The UI explains the refusal in one sentence rather than showing a code.

**Already in place:** `DockerBackend::provision_worktree` is the one place a refusal can be raised before the
first git command; `BackendError::failed(reason)` is how a sentence reaches the row and the transcript;
`src/worktree.rs` holds `Worktree::of` (the repository path), so a `refusal()`-style check belongs beside it
as `Worktree::refusal(&self) -> Result<(), Refusal>` or in the backend, decided by where the reading happens;
`tests/worktree.rs` is where the cases go (a real repository in a `TempDir`); `ApiError`'s constructors are in
`http/error.rs` and its codes are constants.

**What the story has to decide (none of it is settled — plan it before coding):**

1. **422 is an HTTP answer, but the refusal is discovered inside the provisioner's background task**, where
   there is no request to answer. Read the story's wording against the flow: `POST /api/sessions` answers 202
   *before* provisioning. The honest options are (a) the refusal happens in the **create handler**, before the
   202 (a `git` read per create — that is a synchronous check on the request's path and it is what "refused at
   session creation" means), or (b) it stays in the step and reaches the operator through the row's reason
   and a 409-shaped sentence instead of a 422. The spec (item 1) says "Refused with 422 at session creation",
   so (a) is canonical — and the sentence must then not be a `BackendError` but the create's own `ApiError`.
2. **Three new error codes** (`git_not_directory`, `submodules_present`, `worktree_config_enabled`) means
   `http/error.rs` constants, the story's distinct-code acceptance, and a case each — plus the sentence rule
   ("the UI explains the refusal in one sentence rather than showing a code"), which with today's page means a
   sentence in the *create form's* error area (`web/src/main.ts` renders `bad_project` today — find it first).
3. **How to detect each condition without a new dependency**: `.git` is a directory (`Path::is_dir`, but a
   `.git` *file* means a worktree/submodule checkout); submodules (`git -C repo submodule status` is a read,
   or `.gitmodules` existing — decide from what item 1 says the hole is); `extensions.worktreeConfig`
   (`git -C repo config --get extensions.worktreeConfig` is a read that answers non-zero when unset).
4. **Where the check runs so it costs nothing when it can be avoided**: a project that has been created from
   before is checked again on every create unless it is cached, and a cache is a second source of truth for a
   repository that can change under it. Simplest canonical answer: check on every create (one or two `git`
   calls beside a `worktree add` that costs the same order), and say so.
5. **Whether the stub is affected.** A stub create has no repository, so the check must be the Docker backend's
   (or the handler's, gated on `runtime.backend`) — otherwise every stub test and the whole gate would need a
   repository. Read §1 fact 7 and ADR-0015/0028 before choosing.

### What HS-025 and HS-024 settled that HS-026 inherits

- **The worktree step is real and idempotent** (ADR-0029): create-or-adopt, the row's base wins over the
  branch's head, and a step that only adopted is not undone when a later step fails
  (`Provisioned::undoable`). A refusal must therefore run **before** `ensure`, so a refused create leaves
  nothing behind — that is the story's fourth acceptance line.
- **A backend cannot write the row** (ADR-0019); the provisioner writes what a step made. A refusal is not a
  record, so it needs no `Provisioned` variant.
- **The sentence is the deliverable**, not the code: `BackendError::failed` and `ApiError::not_in_state` both
  exist because the operator reads one sentence, and no test pins prose — only that the code names its own
  request.
- **A new `case_*` per story**, and a case that needs a real repository puts one in a `TempDir` (as
  `tests/worktree.rs` does) or boots its own server with `case_worktree`'s fake-`_ping` trick.
- **One build at a time, commit then `verify.sh` then the demo** — and a `web/` edit needs the rebuild before
  any browser check (§3).

## 9. Reporting

Keep a running ledger in the conversation (issue → plan → implement → review → fix → ship, with SHAs) so
a restart can see what was done from Linear, `docs/review/` and `git log`. The Linear issue is the
durable record; the review file is the durable review; the commit history is the durable change. If you
stop before the backlog is empty, say so explicitly, say which issue is next, and leave the tree clean
and the gates green — a half-finished story is worse than an honest stop.

This session's ledger (the twenty-fifth story):

| Issue | Story | Commits | Outcome |
| --- | --- | --- | --- |
| PER-55 | HS-023 token budget | `2e902c2`, `fbc0659` | Done — no P1; 1 spec P2 (the two-turn tally the ADR claimed was never tested, and a `SET` mutant stayed green), 2 spec P3s (the no-usage contingency is "counts what was reported", not "disabled"; a glossary overclaim), 8 standards P3s (a mangled doc comment, a helper spliced into another's heading, a variable with no reader, `WRITER_TICK` restated instead of derived, two parsers disagreeing about what a token count is, a middle-man re-export). The gate also caught a flake in the slice: three tests read the tally right after the turn and went red 2 runs in 5 |
| PER-57 | HS-025 worktree provisioning | `7e36bde`, `1fe7f6e`, `ce4dc82` | Done — Standards **PASS with findings** (1 P1, 4 P2/P3), Spec **PASS with findings** (1 P2). The P1 was real: `case_worktree`'s python3 guard sat after its `stop_serve`, so a machine without python3 failed the gate at `FAIL serve/sigterm` while three sentences promised it passed. The valuable one was **the plan's gap, not the code's**: the provisioner undoes every completed step, and the worktree step's undo deletes the worktree *and* `branch -D`s the branch — so a resumed session's adopted worktree (with the agent's commits on it) would be destroyed when the next step refused. Fixed with `Ensured::created` → `Provisioned::undoable`, proved by mutation. Also fixed: the unreachable `not_yet` arm, four doc sentences, and a host-dependent Docker test. New: `src/worktree.rs`, `tests/worktree.rs` (10 cases), `case_worktree`, ADR-0029 |
| PER-56 | HS-024 Docker preflight | `f241ced`, `0b7d50e` | Done — **BLOCK on the spec axis, and it was right**: the preflight ran *after* `Database::open`, so a refused startup had already migrated (my own scripted block move; the gate case now asserts no `app.db`). Plus 5 real P2s (a `hub` was refused, `permission denied` got the `systemctl` advice, `df`'s own words were dropped, HS-046 misattributed as owning ssh in four places, three documents disagreeing about the stub) and 2 standards P2s (a duplicated fake-daemon fixture, two tests that depended on the host's free space). The first gate run also found that `check-static.sh`'s empty-root run needs `--backend stub` |

The session before it, for the shape of a ledger entry:

| Issue | Story | Commits | Outcome |
| --- | --- | --- | --- |
| PER-48 | HS-016 abort ladder | `d034237`, `3d2ecba` | Done — 2 P2s fixed (the grace did not bound rung 1; `Unreported` read as failure), 6 nits |
| PER-49 | HS-017 steer | `0099ca3`, `8ff1d77` | Done — 1 P1 fixed (a `last_active_at` assertion the *settle* satisfied, proved by mutation), 11 notes |
| PER-50 | HS-018 stopped/resume | `dce53c4`, `ff9d907` | Done — 1 P2 fixed (abort retried a dead path, provisioning a session nobody asked for), 16 findings |
| PER-51 | HS-019 delete flow | `3d5c415`, `7c4ea02`, `cd85572`, `db95a3f` | Done — 1 spec P2 fixed (a refused delete of a running session has already ended the turn; now pinned by a test and by the glossary), 4 doc contradictions, 2 nits; also closed PER-41's recorded batch-collateral nit |
| PER-52 | HS-020 cap and eviction | `a3c6825`, `4f37211`, `f67eb6f` | Done — 1 P1 fixed (the "nothing was deleted" sentence was written only where a row is built, so the frame path an eviction arrives by showed a dash; now one function both call sites use, proved in Chromium on the live path), 2 P2 test/gate fixes (a random-id claim; a cap case that bet on the host's speed), 1 P2 duplicate column list, 1 mangled doc comment, 3 stale sentences; the eviction's `stop` is spawned, not awaited |
| PER-53 | HS-021 idle stop | `5da2ae2` | Done — no P1; 5 P2s and 3 nits, all fixed. The two test-claim findings were settled by mutation (the bound now brackets `(90s, 130s]`, and the candidate query's `status = 'idle'` has a test that reads the query); two review claims about the *plan* and the *ADR* were fixed as documentation, and the gate's process-wide `idle_stop_min = 1` is now a written rule for later cases |
| PER-54 | HS-022 turn timeout | `e767757` | Done — **BLOCK on both axes, both real**: 1 spec P1 (the nested `select!` could cancel the climb between rung 2 and the resume, leaving a session stopped forever — now `Ladder::spawn` plus a resume assertion), 1 standards P1 (`docs/stories/README.md` carried the ADR-0025 bullet twice, this session's scripted-edit bug), plus ADR-0020's amendment note, 3 stale-doc fixes, and a "per turn" test that could not fail (now a second fast turn under the same deadline) |
