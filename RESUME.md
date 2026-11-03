# RESUME — orchestrator handoff (cold start, after fifteen stories)

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
created, prompted, aborted, steered, stopped and resumed, and it is driven from the phone.

---

## 1. Where the work stands

Branch **`master`** (this repo has **no `main`**; do not rename it). The tree is clean. `HEAD` is
**`e17920c`** (a handoff-refresh docs commit); any commit after it is bookkeeping.

**Linear: 15 Done, 65 Backlog, all in project `Holosmith`.** Done is `HS-001` … `HS-015`
(`PER-33` … `PER-47`).

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

**Next issue: `PER-48` (HS-016 — abort with the full ladder).** Then `PER-49` (HS-017 — steer),
`PER-50` (HS-018 — stopped/resume), and onward in `HS-###` order.

Ranking, per `PROMPT.md`: Linear priority (all are `No priority`) → `HS-###` order in `docs/stories/` →
anything already `In Progress` first → skip an issue whose `Blocked by` stories are not `Done`.

### Two facts a cold start must know

1. **A running turn has nothing that can stop it.** `SessionBackend::abort`/`stop`/`resume` are
   recordings in the stub and `unsupported` in Docker; `turn.rs` holds no cancellation token; there is
   no timeout. HS-016 is exactly that gap and it is the most expensive one to leave open.
2. **Nothing has ever rendered the web page.** There is no browser here. The transcript's *rules* are
   covered by node tests; the DOM half is **unverified from source** — say so, never claim a visual
   check.

Two things HS-014 and HS-015 settled that later stories inherit:

- **The transcript's event contract** (ADR-0018 + ADR-0019), emitted by the backend:

  | Kind | Payload |
  | --- | --- |
  | `message.delta` | `{"message_id", "kind": "text"\|"thinking", "text"}` |
  | `message.snapshot` | `{"message_id", "text", "thinking"}` (nothing emits it yet; HS-060) |
  | `message.end` | `{"message_id", "usage": {"in", "out"}}` |
  | `tool.start` / `tool.end` | `{"tool_id", "name", "arguments"}` / `{"tool_id", "name", "output"}` |
  | `turn.start` / `turn.end` | `{}` |
  | `session.status` | `{"from", "to", "step", "reason"}` (ADR-0016, the transitioner only) |

  A message id keys a transcript block; a delta is one appended text node; a snapshot is the only
  replacing operation and is the last frame for its id; an unknown kind is dropped, not refused.

- **The backend emits a turn's frames; the server owns every state change.** `start_turn` takes
  `&EventWriter` for that reason, and no backend can reach a `Transitioner`. The prompt's
  `idle → running` is the *handler's* (so the 202 can promise `running`); the way out (`agent_settled`,
  or `unexpected_exit` with the reason) is `turn.rs`'s. Refusals are one family: `409 session_<state>`
  — `session_starting`, `session_running`, `session_stopped`, `session_error`, `session_deleting` —
  built by `ApiError::not_idle`.

**Pushing is impossible.** `origin` is `git@github.com:AdamOlsson/holosmith.git` and it answers
`Permission denied (publickey)` — re-tested at `e17920c`, so this is a live fact, not a stale note.
There is no ssh key and no `gh`. Every story so far is local commits on `master` plus a closing comment
that says so. Do not fake a push; report the blocker.

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
types natively, which is what `npm test` relies on); `curl`; `python3`; `sed`, `grep`, `tr`, `cut`,
`cmp`, `find`, `timeout`, `git`.
**Not available, and not to be invented:** no Docker daemon, no `sudo`, no `apt`, no `gh`, no ssh key
or token, no **browser** (so nothing renders `web/` — say "unverified" rather than guessing).
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
cargo test                     # 409 tests: 207 lib + 202 across 16 integration binaries
cd web && npm test             # 15 transcript tests (node's own runner)
scripts/verify.sh              # THE one-command gate since HS-006
```

`scripts/verify.sh` runs six phases, cheapest first: `fmt`, `clippy`, `test`, `build`
(`scripts/build.sh`, which itself runs `build-web.sh` — `npm test` then Vite — and `check-static.sh`
last), `embed` (`scripts/check-embed.sh`), then `serve` — the **shipped musl artifact** booted on
`--port 0` with a temp data dir, running **26 cases plus a CORS sweep (98 assertions)**, then torn down
with `SIGTERM` and an asserted exit 0. It takes ~3 min (both musl targets). `--no-build` skips `build`
and `embed`; `--keep` keeps the work dir.

A story is not done until all four are green on a clean tree, and then you run the story's own demo
yourself and paste its output.

**Run one build at a time.** A `scripts/build.sh` or `scripts/verify.sh` in the same tree while another
one runs can make `check-static.sh`'s banner wait flake under load (it happened once during HS-013: the
gate reported `FAIL build/failed` while the same command passed a minute later, alone). Serialise them.

**Rebuild after committing.** `git describe` is baked into the artifact and `check-static.sh` fails on a
stale `dist/`, so a commit without a rebuild leaves the gate red. Commit, then `scripts/verify.sh`, then
the demo you paste.

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
  src/backend/              mod.rs (Step, SessionBackend, Backends, BackendError), stub.rs (the
                            scripted turn), docker.rs (refuses everything until HS-024)
  src/provisioner.rs        the four-step background task and its five session.status events
  src/turn.rs               the turn runner: touch → the backend's turn → agent_settled + touch,
                            or unexpected_exit with the reason
  src/runtime.rs            runtime::Backend {Stub,Docker} — which backend this run uses
  src/http/                 mod.rs, app.rs (Routes/App seam, AppState + provisioner()/turner()/
                            transitioner(), edge layer, is_guard_exempt), error.rs (the one envelope,
                            every code, ApiError::not_idle), health.rs, guard.rs (the four checks),
                            sessions.rs (POST+GET /api/sessions, GET /api/sessions/{id},
                            POST /api/sessions/{id}/prompt), streams.rs (the SSE handler)
  src/sessions/             mod.rs (Status, Session, NewSession, RepoError), repository.rs (every
                            `sessions` statement + its SQL-location guard; includes touch), state.rs
                            (the transition table, Transitioner::apply / apply_logged, the set_status
                            guard)
  src/events/               mod.rs (Event, NewEvent, EventWriter, EventStream), writer.rs, repository.rs,
                            stream.rs (replay→live, lag), registry.rs (open-stream counts)
  src/db.rs                 pragmas + read-back, apply_pending, Database::sessions()/events()
  src/sql.rs                the one SQL statement scanner + the shared source walk (cfg(test) guard)
  src/schema.rs             SCHEMA_VERSION (= 3), include_dir! embed, check_embed, contract_violations
  src/{serve,web,logging,clock,version,pins}.rs
  tests/                    version_cli, config_cli, http_contract, serve_cli, db_migrations,
                            pin_single_source, guards, session_repository, sessions_api, events,
                            streams, provisioning, state_machine, session_list, session_screen,
                            turns, support/mod.rs (the shared harness)
web/                        Vite 7 + TypeScript, two screens (`/` and `/sessions/<id>`);
                            package-lock COMMITTED; main.ts is the DOM, transcript.ts the renderer's
                            rules (frame parsing, block model, affordances)
web/test/transcript.test.ts 15 tests over synthetic frames, node's own runner (`npm test`)
scripts/build-web.sh        npm ci + npm test + npm run build, asserts web/dist/index.html
scripts/build.sh            web first, HOLOSMITH_REQUIRE_WEB=1, both musl targets → dist/<triple>/,
                            atomic copy, then check-static.sh. Uses `cargo build --locked`.
scripts/check-static.sh     file/readelf, empty-root serve + cmp against web/dist/index.html, marker
                            gate, app.db assertions, byte grep for the schema SQL, Cargo.lock guard
scripts/check-embed.sh      the three embed cases (present / absent warns / required fails)
scripts/verify.sh           the one-command gate (see §3)
scripts/import-linear.py    the MCP plumbing /tmp/lin.py reuses
docs/adr/0001…0019          read the recent ones before planning anything
docs/build.md               prerequisites, gates, verify.sh, guards, migrations, placeholders
docs/review/PER-33…PER-47   each a verbatim review + a `## Resolution` table
docs/stories/               the 80 story slices — the dependency order and definition of done
GLOSSARY.md                 the domain vocabulary. Check it BEFORE inventing a term.
```

### What exists and what does not

- **Working:** `version` (text + `--json`), `config {validate,show,edit}`, `serve` (control listener,
  embedded app, `/api/health`, `POST`/`GET /api/sessions`, `GET /api/sessions/{id}`,
  `POST /api/sessions/{id}/prompt`, `GET /api/sessions/{id}/events`), SQLite with three migrations, the
  guards, the event writer, the SSE stream, the stub backend and its four-step provisioner, the enforced
  state machine, the Sessions screen with its create form, the session screen at `/sessions/<id>` with a
  live transcript, and a prompt that runs a scripted turn to `idle`.
- **Not built yet:** abort/steer/resume/delete endpoints (HS-016 … HS-018), the abort ladder and the
  turn timeout (HS-016/HS-022), tool-output files and `output_ref` (HS-033), usage totals (HS-034), the
  preview listener (HS-041), Docker (HS-024+), merge, publish, push, the hub, releases.
- **`runtime::Backend`** has `Stub` and `Docker`; `serve --backend` and the config key `backend` select
  it. `DockerBackend` **refuses every operation** until HS-024, and the default backend is `docker`
  (ADR-0001: the stub is opt-in), so a default build provisions a session to `error` with a reason
  rather than hanging in `starting`. `scripts/verify.sh` boots with `backend = "stub"` in its config.
- **`AppState`** is `{ config, runtime, database: Arc<std::sync::Mutex<Database>>, events: EventWriter,
  streams: StreamRegistry, backend: Backends }`, with three wiring helpers — `provisioner()`,
  `turner()`, `transitioner()`. The mutex is an explicit stopgap (ADR-0012/0013) — see §6.
- **`sessions::state::Transitioner::apply`** is the only way a session's state changes; `apply_logged`
  is the runners' wrapper (log, never return). A provisioning step report is `from == to` with `step`
  set and is *not* a table row.
- **`config`** already has `abort_grace` (default `10s`), `idle_stop_min`, `stopped_ttl_days`,
  `min_free_gb` and the rest — check `config show` before adding a key. HS-016 **uses** `abort_grace`.

---

## 5. The per-issue cycle (what actually worked, fifteen times)

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
   - **S/M with the analysis in hand** (HS-011, HS-012, HS-014): implement in-session.
   - **M/L with a lot of new surface** (HS-013's screen, HS-015's turn): hand a fresh `worker` child a
     *written* brief — the plan plus the "how", the ground rules (no commit, no Linear, no new
     dependency, `env.sh` first, don't edit `docs/stories/**`), the exact gate commands, and "budget
     discipline: keep the tree compiling; if you run long, stop green and report which steps remain".
     Launch it async; native completion wakes you. Twice now the child found a genuine contradiction in
     my brief — expect that, and verify its claims rather than trusting them.
5. **Verify yourself.** Never trust a child's pasted output: run the gates and the demo. Do this every
   time; it has caught something each story (a wrong test count, a demo-revealed message bug, a panic
   path).
6. **Review** with a fresh `reviewer` child running `~/.agents/skills/code-review/SKILL.md`, read-only,
   against `<fixed-point>...HEAD` plus untracked files, with the spec axis sourced from the Linear body
   + story slice + spec items. `git add -A` first, then `git diff --cached <fp> > /tmp/holosmith/reviews/
   PER-<n>-diff.txt` so new files appear. **The reviewer child has no shell or write tool**: its claims
   must be checked, and *you* save the review verbatim to `docs/review/PER-<n>.md`.
7. **Fix** in the same session (or with a fresh `worker`), triaging every finding as fixed or
   dismissed-with-citation ("the reviewer misread the helper at …"). Re-run the gates. Append a
   `## Resolution` table to the review file and commit it with the fixes.
8. **Ship**: focused conventional commits naming the issue, the last ending `Closes <IDENT>`. Rebuild
   after committing. Then `save_comment` with what shipped, the gates, verbatim demo output, acceptance
   item by item, and follow-ups; set the issue `Done`; re-read it to confirm.
9. **Refresh this file** at a natural stopping point (counts, HEAD, next issue, the next story's facts).
   It is a cold-start snapshot, not a per-story deliverable — one refresh per session is enough.

Commit messages are prose about *why*, not a restatement of the diff. Look at the last twenty for tone.

Working artifacts (`/tmp` is ephemeral; the durable record is Linear + `docs/review/` + `git log`):

```text
/tmp/holosmith/plans/PER-<n>.md           the plan
/tmp/holosmith/plans/PER-<n>-linear.md    the Linear comment body
/tmp/holosmith/plans/PER-<n>-impl-brief.md
/tmp/holosmith/plans/PER-<n>-ship.md      the closing Linear comment body
/tmp/holosmith/reviews/PER-<n>-diff.txt   the diff artifact for the reviewer
/tmp/holosmith/reviews/PER-<n>-review-brief.md
/tmp/holosmith/reviews/PER-<n>-review-verbatim.md
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
- **No `awk`, no `shellcheck`, no browser.** Scripts must use `sed`/`grep`/`tr`/`cut`/`cmp`/`find`/
  `timeout`; UI claims you cannot render are "unverified from source", not "done".
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
- **A test that asserts only a status is how a wrong code ships.** Assert status **and** `code` **and**
  `content-type`, and prove a new guard fires by removing the behaviour and watching the test go red.
- **The harness's stub is deliberately slow** (`SLOW_STUB_STEP` = 60 s per step) so no test that is not
  about provisioning loses the race against HS-011's background task. Tests that need provisioning to
  finish use `support::with_stub(state, support::fast_stub())` (1 ms). Waiting for a row to reach `idle`
  is `wait_until(...)`, never a `sleep`.
- **A row can be ahead of its event.** The runner writes first and the writer commits every 25 ms, so
  "the row says `idle`" does not mean the event is committed. Tests read the log by event count, and the
  stream tests take N frames.
- **A prompt turns a race into an expected outcome.** Two prompts can both see `idle`; the loser gets
  `StateError::Illegal` and `apply_prompt` retries once when the row is idle again. Do not "fix" that by
  holding the lock across the read and the apply.

### HTTP and the envelope

- **Every fallible axum extractor answers `text/plain`, not the envelope** — `axum::Json<T>`'s
  rejection, `Bytes`'s (`UnknownBodyError` 400, `LengthLimitError` 413) and `Path<String>`'s (a `%FF`
  segment is a 400). Take the body and the path as `Result<…, Rejection>` and map them yourself.
- **The `Routes`/`App` seam is load-bearing**: `Routes` has no constructor from a bare `Router` and no
  accessor yielding one, `axum::serve` appears once, and `Router::layer` runs in exactly one place —
  `App::new`. `tests/guards.rs` pins the one `.layer(` and the manifest's read-only/mutating kinds.
- **A `GET` route cannot be mutating.** Use `Routes::get`/`head`/`post`/…; the manifest records the kind.
- **Errors are one envelope**, `{"code","message"}` (ADR-0008); codes are constants in `http/error.rs`.
  No test pins a message sentence — but a message must describe *its own* request (HS-015's prompt
  reused the create's sentence until a demo showed it).
- **`is_guard_exempt` is the only exemption list** (true only for `GET`/`HEAD /api/health`).
- **Banner line order is pinned by tests** (`log`, version line, `config`, `data_root`, `control_listen`,
  `control_origin`, `preview_listen`, `database … schema n`).
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
- **The repository owns every statement against its table**, enforced by a walk over `src/**`.
- **No state changes outside `sessions::state::Transitioner::apply`.** `set_status` may be *named* only
  in `sessions/repository.rs` and `sessions/state.rs`; a walk over `src/**` enforces it. Integration
  tests outside the crate may still call the raw write (they test the repository).
- **The `session.status` payload is `{from, to, step, reason}`**, four keys, explicit `null`s. A step
  report is `from == to` with `step` set and writes nothing; a transition carries `step: null`.
- **The create's initial state is the table's `none` row**, not a second spelling of `starting`.
- **A backend can emit an event but cannot write the row.** Its `EventWriter` takes free-form
  `kind`/`payload`, so a *forged* `session.status` frame is possible and is exactly the lie ADR-0016
  forbids. ADR-0019 records it as a known hole; closing it means typing the payload (no story yet).

### Web

- **No new npm dependency, no framework, no router.** `EventSource`, `crypto`, `fetch` are the platform;
  `package-lock.json` is committed; `npm test` is node's own runner.
- **`web/src/main.ts` keeps no state beyond the DOM** (plus the create's pending Idempotency-Key). The
  transcript's rules live in `web/src/transcript.ts` and are tested without a browser.
- **A delta is one appended text node; a snapshot is `replaceChildren`.** Never `textContent +=` (O(n)
  per delta ⇒ O(n²) per turn), never a document query per frame — blocks are a `Map<message id, nodes>`,
  tools a `Map<tool id, nodes>`.
- **Never `innerHTML` with server data.** Every node is `createElement` + `textContent`.
- **A form inside a page must swallow its own submit** — with no `action` it is a GET of the current path
  and a full reload, which discards the transcript (HS-014's review caught exactly that).
- **The session screen keeps its stream open in every state**, unlike a list row (ADR-0017 closes a dead
  row's source). Both are deliberate; ADR-0018 says why.
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
  `docs/build.md`, and the reviewer enforces it. The current gate is 26 cases / 98 assertions.
- **Adding an ADR means updating `docs/stories/README.md`**: the "Decisions taken while slicing" count
  (now nineteen) and the bullet list. A reviewer will check the links resolve — three did not, once.
- **An ADR that a later story changes gets a one-line "Superseded by ADR-00xx" note**, not a rewrite
  (ADR-0012 does this; HS-015 added two to ADR-0018).
- **`GLOSSARY.md` is the vocabulary authority.** Check it before inventing a term. Recent entries to
  respect: **Step**, **Backend**, **Trigger**, **Transition**, **Sessions screen**, **Session screen**,
  **Composer**, **Transcript**, **Delta kind**, **Message id**, **Turn boundary**, **Turn runner**,
  **Tool call**, **Usage**. `Status` in code is **State** in the glossary; a delta's node is a "kind's
  node", never a "channel"; a second spelling is forbidden, including inside an entry's own prose.

### Reviews

- **Reviews are saved verbatim** and a `## Resolution` section is appended afterwards. Do not edit the
  review body; do not delete a dismissal.
- **The reviewer has no shell.** Its claims must be checked: HS-014 and HS-015 each had one finding the
  reviewer got right and one it got wrong (it read a helper as the wrong function; it called an
  `unreachable!()` path unreachable when a race reached it).
- It reliably finds one real should-fix per story. Take them seriously.

### Demos

- **Demo commands must be bounded.** Background a server with every fd redirected and `timeout`, and kill
  it by its own pid. A backgrounded server keeps the tool call's stdout open and hangs the child.
  `pgrep -f 'holosmith serve'` matches unrelated processes.
- **Launch demos from a script file, not an inline `&` chain** — `cd … && … && binary serve &` puts
  the whole chain in a subshell, so `$!` is not the server's pid and `wait` reports the shell's status.
- **`scripts/verify.sh`'s `fetch` waits for EOF**, which an SSE stream never sends. Use `fetch_stream`
  or `fetch_sse_until`.
- **A create's state is not stable in a demo**: the provisioner runs 200 ms/step under `--backend stub`,
  so a `GET /api/sessions` a second later shows `idle`, not `starting`. Assert what the answer *is*.

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

---

## 8. Resume checklist

```bash
cd /home/adamolsson/projects/holosmith
git status --short                          # must be clean
. ~/.pi/toolchains/env.sh
python3 /tmp/lin.py issues | grep -v ' Done ' | head   # next unfinished (recreate /tmp/lin.py, §2)
python3 /tmp/lin.py issue PER-48            # sanity-check Linear is reachable and scoped
scripts/verify.sh                           # baseline green (~3 min, 26 cases, 98 assertions)
```

Then start the cycle at **`PER-48` (HS-016 — abort with the full ladder)**. Read §1's two facts first: a
running turn currently cannot be stopped, and nothing has ever rendered the page.

### What HS-016 is about, so the first brief is fast

Acceptance, verbatim from `docs/stories/phase-1-session-lifecycle-stub.md:165`:

- `POST /api/sessions/:id/abort` returns 202 immediately; progress arrives as events.
- Rung 1: `clear_queue` then `abort` over the backend RPC.
- Rung 2: if not idle after `abort_grace` (10 s), force-stop → `stopped` with reason `forced_abort`,
  then immediate resume → `idle`.
- Abort completing normally moves `running → idle`.
- The ladder is a shared routine with a parameterised reason, because HS-022 and HS-023 reuse it.
- A test drives the stub to ignore rung 1 and asserts rung 2 fires and the session ends `idle`.

**Already in place:** the table's `running + abort_completed → idle`, `running + abort_ladder →
stopped` and `stopped + resume → starting`; `SessionBackend::abort`/`stop`/`resume` (recordings in the
stub, `unsupported` in Docker); two runners to copy (`provisioner.rs`, `turn.rs`); the `409
session_<state>` family; the `session.status` payload; and **`abort_grace` is already a config key**
(default `10s`) — use it, do not add one.

**What is missing:** the cancellation. The turn runner holds no token, and the stub cannot be told to
ignore rung 1, so the ladder has nothing to climb.

**Decide canonically and record:** how a turn is cancelled (a shared token the stub's script checks
between frames, versus the backend owning it — the token keeps "the backend emits, the server decides"
intact because the runner still applies the transitions); how the stub is told to ignore rung 1 (a
constructor argument like HS-011's step delay, **not** a config key); whether the forced rung resumes
inline or through a runner; whether an abort on `idle`/`stopped` answers `409 session_<state>` or 202
with no work (the family says the former); and `clear_queue` — the trait has no such method, so say
whether it is a new backend method or folded into `abort`.

---

## 9. Reporting

Keep a running ledger in the conversation (issue → plan → implement → review → fix → ship, with SHAs) so
a restart can see what was done from Linear, `docs/review/` and `git log`. The Linear issue is the
durable record; the review file is the durable review; the commit history is the durable change. If you
stop before the backlog is empty, say so explicitly, say which issue is next, and leave the tree clean
and the gates green — a half-finished story is worse than an honest stop.

Last session's ledger, for the shape of it:

| Issue | Story | Commits | Outcome |
| --- | --- | --- | --- |
| PER-46 | HS-014 session screen | `82c8c35`, `527931a` | Done — 2 P1 findings from the review, both fixed |
| PER-47 | HS-015 prompt/turn | `d4077df`, `e17920c` | Done — 1 P1 (ids reset per process) fixed, one dismissal cited |
