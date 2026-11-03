# RESUME — orchestrator handoff (cold start)

You are the **orchestrator for implementing Holosmith**. Read `PROMPT.md` first: it is the standing
operating procedure and it still governs. This file is the delta — the state of the world, the
environment repairs already made, the decisions taken on the operator's behalf, and the landmines
that cost real time. Read it all before touching anything.

The run is **fully automatic**. The operator is not present. Never ask them a question; decide
canonically, record the decision in the Linear issue and the commit message, and continue.

---

## 0. What this project is

`HolosmithSpec.md` is the authoritative specification. It is a single-operator system that runs coding
agents in containers on machines you own and lets you drive them from a phone. The work is sliced into
**80 stories** (`HS-001` … `HS-080`) in `docs/stories/`, one Linear issue each, in five phases:

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

---

## 1. Where the work stands

Branch **`master`** (this repo has **no `main`**; do not rename it). The tree is clean. `HEAD` is
**`82c8c35`** (HS-014); the commit after it is this handoff refresh.

**Linear: 14 Done, 66 Backlog, all in project `Holosmith`.** Done is `HS-001` … `HS-014`
(`PER-33` … `PER-46`).

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

**Next issue: `PER-47` (HS-015 — prompt and the turn lifecycle).** Then `PER-48` (HS-016 — abort),
`PER-49` (HS-017 — steer), `PER-50` (HS-018 — stopped/resume), and onward in `HS-###` order.

Ranking, per `PROMPT.md`: Linear priority (all are `No priority`) → `HS-###` order in `docs/stories/`
→ anything already `In Progress` first → skip an issue whose `Blocked by` stories are not `Done`.

### What HS-014 settled, and two facts about HS-015 that a cold start must know

HS-014 shipped the session screen at `/sessions/<id>` (read from `location.pathname`), grew
`GET /api/sessions/{id}` (the bare row, `not_found` for anything else), and **pinned the transcript's
event contract in ADR-0018**, because its producer does not exist yet:

| Kind | Payload |
| --- | --- |
| `message.delta` | `{"message_id", "kind": "text"\|"thinking", "text"}` |
| `message.snapshot` | `{"message_id", "text", "thinking"}` — both kinds default to `""` |
| `turn.start` / `turn.end` | `{}` — no payload is read |

A message id is the block key; a delta is one appended text node; a snapshot replaces both kind
nodes; a snapshot is the last frame for its id; an unknown kind is dropped, not refused. The rules
live in `web/src/transcript.ts` and are tested by `web/test/transcript.test.ts` with node's own
runner (`npm test`, wired into `scripts/build-web.sh`, so it runs inside the gate).

1. **The stub still emits no message.** HS-015 owns the scripted turn, so HS-014's own demo line
   ("prompt a session, watch text and thinking arrive") has **never run**, and the DOM half of the
   screen is unverified from source — this machine has no browser. HS-015 is the first story whose
   demo can look at it, so start its demo with the session screen open.
2. **The composer is wired to nothing, on purpose.** Prompt, steer and abort exist per
   `affordances(state)` — prompt on `idle`, steer and abort on `running` — and pressing one names the
   story that will wire it (prompt HS-015, abort HS-016, steer HS-017). The form swallows its own
   submit; HS-015 replaces that handler with the real request.

**Pushing is impossible.** `origin` is `git@github.com:AdamOlsson/holosmith.git` and it answers
`Permission denied (publickey)` — re-tested after HS-013, so this is a live fact, not a stale note.
There is no ssh key and no `gh`. Every story so far is local commits on `master` plus a closing
comment that says so, and every Linear note repeats it. Do not fake a push; report the blocker.

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

**Available:** node 24.22.2 / npm 11.19.1 with the npm registry reachable; `curl`; `python3`; `sed`,
`grep`, `tr`, `cut`, `cmp`, `find`, `timeout`, `git`.
**Not available, and not to be invented:** no Docker daemon, no `sudo`, no `apt`, no `gh`, no ssh key
or token, no **browser** (so nothing renders `web/` — say "unverified" rather than guessing).
**No `awk`** and **no `shellcheck`** on this machine — do not write a script or a gate that needs
either.

### Linear access

There are no Linear tools in the tool list. `list_issues` / `save_issue` / `save_comment` / `get_issue`
are reachable through the same HTTP MCP endpoint `scripts/import-linear.py` uses, so drive it from a
wrapper. Recreate `/tmp/lin.py` if it is gone:

```python
#!/usr/bin/env python3
# Thin CLI over the Linear MCP server, reusing scripts/import-linear.py plumbing.
#   python3 /tmp/lin.py issues            # id, title, status
#   python3 /tmp/lin.py issue PER-46      # full issue JSON
#   python3 /tmp/lin.py comment PER-46 FILE
#   python3 /tmp/lin.py state PER-46 "In Progress"
#   python3 /tmp/lin.py states            # the team's states
#   python3 /tmp/lin.py call get_issue '{"id":"PER-46"}'
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
cargo test                     # 395 tests: 204 lib + 191 across 16 integration binaries
cd web && npm test             # 12 transcript tests (node's runner; also run by build-web.sh)
scripts/verify.sh              # THE one-command gate since HS-006
```

`scripts/verify.sh` runs six phases, cheapest first: `fmt`, `clippy`, `test`, `build`
(`scripts/build.sh`, which itself runs `build-web.sh` — `npm test` then Vite — and `check-static.sh`
last), `embed` (`scripts/check-embed.sh`), then `serve` — the **shipped musl artifact** booted on
`--port 0` with a temp data dir, running **23 cases plus a CORS sweep (82 assertions)**, then torn
down with `SIGTERM` and an asserted exit 0. It takes ~3 min (both musl targets). `--no-build` skips `build` and `embed`;
`--keep` keeps the work dir.

A story is not done until all four are green on a clean tree, and then you run the story's own demo
yourself and paste its output.

**Run one build at a time.** A `scripts/build.sh` or `scripts/verify.sh` in the same tree while another
one runs can make `check-static.sh`'s banner wait flake under load (it happened once during HS-013: the
gate reported `FAIL build/failed` while the same command passed a minute later, alone). Serialise them.

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
  src/config/               mod.rs (typed Config, KeySpec walk), schema.rs (26 keys), problem.rs,
                            render.rs, duration.rs, editor.rs
  src/backend/              mod.rs (Step, SessionBackend, Backends, BackendError), stub.rs, docker.rs
  src/provisioner.rs        the four-step background task and its five session.status events
  src/runtime.rs            runtime::Backend {Stub,Docker} — which backend this run uses
  src/http/                 mod.rs, app.rs (Routes/App seam, AppState + provisioner()/transitioner(),
                            edge layer, is_guard_exempt), error.rs (the one envelope + every code),
                            health.rs, guard.rs (the four checks), sessions.rs (POST + GET
                            /api/sessions + GET /api/sessions/{id}, each path taken as a Result so
                            a bad segment is the envelope), streams.rs (the SSE handler)
  src/sessions/             mod.rs (Status, Session, NewSession, RepoError), repository.rs (every
                            `sessions` statement + its SQL-location guard), state.rs (the transition
                            table, SessionStatus, Transitioner::apply, the set_status guard)
  src/events/               mod.rs (Event, NewEvent, EventWriter, EventStream), writer.rs, repository.rs,
                            stream.rs (replay→live, lag), registry.rs (open-stream counts)
  src/db.rs                 pragmas + read-back, apply_pending, Database::sessions()/events()
  src/sql.rs                the one SQL statement scanner + the shared source walk (cfg(test) guard)
  src/schema.rs             SCHEMA_VERSION (= 3), include_dir! embed, check_embed, contract_violations
  src/{serve,web,logging,clock,version,pins}.rs
  tests/                    version_cli, config_cli, http_contract, serve_cli, db_migrations,
                            pin_single_source, guards, session_repository, sessions_api, events,
                            streams, provisioning, state_machine, session_list, session_screen,
                            support/mod.rs (shared harness)
web/                        Vite 7 + TypeScript, two screens (`/` and `/sessions/<id>`);
                            package-lock COMMITTED; the one screen module is web/src/main.ts and
                            the renderer's rules are web/src/transcript.ts
web/test/transcript.test.ts 12 tests over synthetic frames, node's own runner (`npm test`)
scripts/build-web.sh        npm ci + npm test + npm run build, asserts web/dist/index.html
scripts/build.sh            web first, HOLOSMITH_REQUIRE_WEB=1, both musl targets → dist/<triple>/,
                            atomic copy, then check-static.sh. Uses `cargo build --locked`.
scripts/check-static.sh     file/readelf, empty-root serve + cmp against web/dist/index.html, marker
                            gate, app.db assertions, byte grep for the schema SQL, Cargo.lock guard
scripts/check-embed.sh      the three embed cases (present / absent warns / required fails)
scripts/verify.sh           the one-command gate (see §3)
docs/adr/0001…0018          read the recent ones before planning anything
docs/build.md               prerequisites, gates, verify.sh, guards, migrations, placeholders
docs/review/PER-33…PER-45   each a verbatim review + a `## Resolution` table
docs/stories/               the 80 story slices — the dependency order and definition of done
GLOSSARY.md                 the domain vocabulary. Check it BEFORE inventing a term.
```

### What exists and what does not

- **Working:** `version` (text + `--json`), `config {validate,show,edit}`, `serve` (control listener,
  embedded app, `/api/health`, `POST /api/sessions`, `GET /api/sessions`, `GET /api/sessions/{id}`,
  `GET /api/sessions/{id}/events`), SQLite with three migrations, the guards, the event writer, the SSE
  stream, the stub backend and its four-step provisioner, the enforced state machine, the Sessions
  screen with its create form, and the session screen at `/sessions/<id>` with a live transcript.
- **Not built yet:** any turn or message producer (HS-015+), prompt/steer/abort/resume/delete
  endpoints, the preview listener (HS-041), Docker (HS-024+), merge, publish, push, the hub, releases.
- **`runtime::Backend`** has `Stub` and `Docker`; `serve --backend` and the config key `backend` select
  it. `backend::SessionBackend` is what a backend must do. `DockerBackend` **refuses every operation**
  until HS-024, and the default backend is `docker` (ADR-0001: the stub is opt-in), so a default build
  provisions a session to `error` with a reason ("the worktree step: the docker backend does not
  provision yet (HS-024)") rather than hanging in `starting`. `scripts/verify.sh` boots with
  `backend = "stub"` in its config file.
- **`AppState`** is `{ config, runtime, database: Arc<std::sync::Mutex<Database>>, events: EventWriter,
  streams: StreamRegistry, backend: Backends }`, with two wiring helpers — `provisioner()` and
  `transitioner()`. The mutex is an explicit stopgap (ADR-0012/0013) — see §6.
- **`sessions::state::Transitioner::apply`** is the only way a session's state changes. It writes the
  row (with `stopped_at`) and then emits `session.status` with `{from, to, step, reason}`. A
  provisioning step report is `from == to` with `step` set, and is *not* a table row.
- **`web/src/main.ts`** is the Sessions screen: the health header, one row per session patched live by
  one `EventSource` per visible live session, and the create form with an idempotency key. No
  framework, no router, no state library, no new dependency.

---

## 5. The per-issue cycle (what actually worked, thirteen times)

Everything is **sequential: one writer at a time**, never two children editing files.

1. **Pick** the issue (ranking above) and read its Linear body plus the matching `docs/stories/` slice.
   Note the HEAD SHA — that is the review's fixed point.
2. **Plan.** For **L** stories, use a fresh `delegate` child briefed to run the grilling protocol
   (`~/.agents/skills/grilling/SKILL.md` + `domain-modeling`), which puts **one** round of frontier
   questions to *you* via `contact_supervisor`; answer with
   `subagent({action:"reply", ...})`/`subagent_supervisor({action:"reply", replyTo, message})`. For
   **S/M** stories, write the plan yourself — it is faster and the analysis is usually already done. In
   either case: state the environment facts, name the files to read, list the decisions already settled
   so they are not re-litigated, and **pre-answer the obvious round-2 questions** — that is what kept
   these stories to one round. The planner may write only `GLOSSARY.md` and a new `docs/adr/*.md`.
3. **Record in Linear before any code exists**: `save_comment` with the plan shape, a
   decisions→precedent table, acceptance restated as testable checks, non-goals, docs touched, the
   demo, and follow-ups; then set the issue `In Progress`. Keep it self-contained but not enormous —
   later readers are the operator and the next orchestrator.
4. **Implement.** Two patterns, both proven:
   - **S/M with the analysis already in hand** (HS-011, HS-012): implement in-session. You hold the
     plan; a child would only re-read the same files.
   - **M/L with a lot of new surface** (HS-013's screen and form): hand a fresh `worker` child a
     *written* brief — the plan plus the "how", the ground rules (no commit, no Linear, no new
     dependency, `env.sh` first), the exact gate commands, and "budget discipline: keep the tree
     compiling; if you run long, stop green and report which steps remain". Launch it async; native
     completion wakes you. It worked first time.
5. **Verify yourself.** Never trust a child's pasted output: run the gates and the demo. This has
   caught real problems every single time (and once caught a *flake*: two builds at once).
6. **Review** with a fresh `reviewer` child running `~/.agents/skills/code-review/SKILL.md`, read-only,
   against `<fixed-point>...HEAD` plus untracked files, with the spec axis sourced from the Linear body
   + story slice + spec items. Write the diff to a file first (`git diff <fp>`; if you `git add -A`
   first, new files appear as `new file mode` entries and `git ls-files --others` is empty — either is
   fine, but check the artifact contains them). **The reviewer child has no write tool in this
   environment**: it returns the review as its answer, so *you* must save it verbatim to
   `/tmp/holosmith/reviews/PER-<n>-review-verbatim.md` and then `docs/review/PER-<n>.md`.
7. **Fix** in the same session (or with a fresh `worker`), triaging every finding as fixed or
   dismissed-with-citation. Where a guard was added or changed, prove it fires by planting an offender
   and watching the test go red. Re-run the gates. Then append a `## Resolution` table to the review
   file (finding → fixed/dismissed → how/why) and commit it with the fixes.
8. **Ship**: focused conventional commits naming the issue, the last one ending `Closes <IDENT>`
   (or, as this run did, a commit body that names it — Linear is closed by the `Done` state).
   Rebuild after committing — `git describe` changes and `check-static.sh` fails on a stale `dist/`.
   Then `save_comment` with what shipped, the gates, verbatim demo output, acceptance item by item, and
   follow-ups; set the issue `Done`; re-read it to confirm.

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
  fails if the literal appears in a second tracked file — including `docs/review/*.md`. A commit message
  is fine; a doc file is not. Every saved review and Resolution table deliberately never spells it out.
- **Never export `RUSTFLAGS`.** It silently disables the per-target `rustflags` in `.cargo/config.toml`
  and ships static-PIE. `check-static.sh` asserts the resulting property so a mistake fails loudly.
- **`scripts/build.sh` builds with `cargo build --release --locked`.** Adding a dependency without
  regenerating `Cargo.lock` fails the whole gate with "the lock file needs to be updated". Regenerate,
  check the lock diff shows only the edge you meant, then run the gate.
- **A bare `cargo build` without `web/dist` succeeds with a placeholder page.** `build.sh` sets
  `HOLOSMITH_REQUIRE_WEB=1`, and `check-static.sh` greps the artifact for the marker. Build the web app
  first or your demo shows the placeholder.
- **`git describe` and `dist/`.** After a commit, `dist/` is stale and `check-static.sh` fails by design.
  Rebuild after committing, before the demo you paste into Linear.
- **No `awk`, no `shellcheck`, no browser.** Scripts must use `sed`/`grep`/`tr`/`cut`/`cmp`/`find`/
  `timeout`; UI claims you cannot render are "unverified from source", not "done".
- **One build at a time.** Two `scripts/build.sh`/`verify.sh` runs in one tree can flake the banner wait
  in `check-static.sh` (fixed 200 × 50 ms). If the gate says `FAIL build/failed`, re-run it alone before
  believing it.

### Tests

- **Test isolation.** Three `serve` tests once created and migrated the *real* `/var/lib/holosmith` and
  the gates passed, because this sandbox lets an unprivileged user write `/var/lib`. Every test that
  starts `serve` must give it a temp `data_root` inside its own temp dir.
- **Two `Database::open`s on one path in one process is ADR-0009's measured "database is locked" trap.**
  A shared test harness must open exactly one (`OnceLock`), and a per-test harness must always take its
  own `TempDir`. `tests/support/mod.rs` carries the pid in the directory name.
- **Same-process SQLite contention bypasses the busy handler.** A two-thread concurrency test is flaky
  by construction; test the lock with a second *process*.
- **A guard test that shares a scratch directory with another guard flakes.** The two SQL-location guards
  and HS-012's `set_status` guard share `sql::guard`; a directory name comes from its own table word (or
  its own label), and `TempDir::new`/`Guard::temp_dir` clear only their own path.
- **A rule that walks `src/**` must prove it found something.** HS-012's `set_status` guard asserts the
  walk saw both allowed files *before* asserting no offenders — otherwise a broken walk passes green.
  Share one matcher between the rule and its planted-offender test, or weakening the rule passes both.
- **A test that asserts only a status is how a wrong code ships.** HS-005's review found a test that had
  been silently asserting `bad_client` while labelled `bad_origin`. Assert status **and** `code` **and**
  `content-type`, and prove a new guard fires by removing the behaviour and watching the test go red.
- **The harness's stub is deliberately slow** (`SLOW_STUB_STEP` = 60 s per step) so no test that is not
  about provisioning loses the race against HS-011's background task. Tests that need provisioning to
  finish use `support::with_stub(state, support::fast_stub())` (1 ms). Waiting for a row to reach `idle`
  is `wait_until(...)`, never a `sleep`.
- **A row can be ahead of its event.** The provisioner/state function writes first and the writer
  commits every 25 ms, so "the row says `idle`" does not mean the event is committed. Tests that read
  the log wait for a count (`committed_status_events`), and tests reading the stream take N frames.

### HTTP and the envelope

- **Every fallible axum extractor answers `text/plain`, not the envelope.** `axum::Json<T>`'s rejection
  and the `Bytes` extractor's (`UnknownBodyError` 400, `LengthLimitError` 413) all bypass ADR-0008. Take
  the raw body and parse it yourself, and take extractors as `Result<T, Rejection>`.
- **The `Routes`/`App` seam is load-bearing**: `Routes` has no constructor from a bare `Router` and no
  accessor yielding one, `axum::serve` appears once, and `Router::layer` runs in exactly one place —
  `App::new`. `tests/guards.rs` pins the one `.layer(` and the manifest's read-only/mutating kinds.
- **A `GET` route cannot be mutating.** `Routes::route` was deleted; `get`/`head` record `ReadOnly` and
  `post`/`put`/`patch`/`delete` record `Mutating`. Use the helpers.
- **Errors are one envelope**, `{"code","message"}` (ADR-0008); codes are constants in `http/error.rs`.
  No test pins a message sentence. A 500's message must be a **fixed sentence**.
- **`is_guard_exempt` is the only exemption list** (true only for `GET`/`HEAD /api/health`).
- **Banner line order is pinned by tests** (`log`, version line, `config`, `data_root`,
  `control_listen`, `control_origin`, `preview_listen`, `database … schema n`).
- **`--port 0` re-derives `control_origin` from the bound port** after `bind`; the guard compares the
  browser's `Origin` as text, which is what makes it work in tests and demos.
- **A `{id}` capture is axum 0.8's syntax**; the spec writes `:id`. `/api/sessions` (the collection) and
  `/api/sessions/{id}/events` are both registered; the former is `ReadOnly`.

### Data and state

- **A seq counter must never be derived from `MAX(seq)`.** Compaction (HS-060) deletes event rows, so the
  counter is the durable `sessions.last_seq`, bumped inside the same transaction as the insert.
- **The event writer has one broadcast channel for all sessions**, so any per-session consumer must
  filter by `session_id` before comparing seqs — otherwise one session's frames reach another's response
  and a foreign seq corrupts the resume point (this was HS-010's blocker).
- **Broadcast strictly after `COMMIT`**, and prove it with a second connection reading the row inside
  the subscriber — counting rows is vacuous.
- **`id TEXT PRIMARY KEY` is nullable in SQLite** (measured on the bundled 3.50.2). The invariant is the
  repository's, not the schema's.
- **`SCHEMA_VERSION` is a literal** and `check_embed` fails the build if it disagrees with the embedded
  files. Bump it in the same commit as the migration. `ALTER TABLE … ADD COLUMN` goes **last**.
- **The repository owns every statement against its table**, enforced by a test that walks `src/**`.
- **No state changes outside `sessions::state::Transitioner::apply`.** `set_status` may be *named* only
  in `sessions/repository.rs` and `sessions/state.rs`; a walk over `src/**` enforces it. Integration
  tests outside the crate may still call the raw write (they are testing the repository) — that is the
  crate boundary, not an exemption. `stopped_at` moves inside the same `UPDATE`.
- **The `session.status` payload is `{from, to, step, reason}`**, four keys, explicit `null`s. A step
  report is `from == to` with `step` set and writes nothing; a transition carries `step: null`. A
  refusal (`StateError::Illegal`) writes nothing and logs from + trigger (a refused request has no
  `to` — the table decides it).
- **The create's initial state is the table's `none` row** (`state::initial_status()`), not a second
  spelling of `starting`.

### Web

- **`web/src/main.ts` holds no state beyond the DOM**, one `EventSource` per visible live session,
  closed on `stopped`/`error`/`deleting` and `beforeunload`. Frames are applied in arrival order; the
  stream replays from the beginning, so the last frame wins and the page needs no seq.
- **Never `innerHTML` with server data.** Agent text is untrusted; every node is `createElement` +
  `textContent`.
- **`crypto.randomUUID` needs a secure context** and the control listener is plain HTTP unless it sits
  behind `tailscale serve`; the page falls back to one `getRandomValues` v4 (same CSPRNG) and shows the
  reason in the form's error line if neither exists.
- **A page cannot set `Last-Event-ID`.** `EventSource` resends the last SSE `id` it saw; the browser owns
  that, not the page.
- **A delta is one appended text node; a snapshot is `replaceChildren`.** Never `textContent +=` (O(n)
  per delta ⇒ O(n²) per turn), never a document query per frame — blocks are a `Map<message id,
  nodes>`. A form inside a page must swallow its own submit: with no `action` it is a GET of the
  current path and a full reload (HS-014's review caught exactly that).
- **The session screen keeps its stream open in every state**, unlike a list row (ADR-0017). Both are
  deliberate; ADR-0018 says why.
- **`case_app_shell` compares `GET /` byte-for-byte with `web/dist/index.html`**, so a stale `dist/` or a
  changed `index.html` fails the gate. `npm run build` is `tsc --noEmit && vite build`.
- **No new npm dependency.** `EventSource`, `crypto`, `fetch` are the platform; `package-lock.json` is
  committed.

### Config and docs

- **Config output** uses `# <key> unset` rather than omitting an unset optional, and `config show`
  output must re-validate clean. `tests/golden/config-default.toml` pins the exact output, so **a new
  config key means updating the golden file and `KNOWN_KEYS.len()`** (currently 26).
- **`docs/build.md` is a story deliverable, not orchestrator-only.** If a story changes prerequisites or
  gates, the doc is in scope, and its prose must be reflowed (~100 columns), not just appended to.
- **Every story adds at least one `case_*` to `scripts/verify.sh`.** The rule is in bold in
  `docs/build.md`, and the reviewer enforces it. The current gate is 23 cases / 82 assertions.
- **Adding an ADR means updating `docs/stories/README.md`**: the "Decisions taken while slicing" count
  (now eighteen) and the bullet list. A reviewer will check the links resolve — three did not, once.
- **`GLOSSARY.md` is the vocabulary authority.** Check it before inventing a term. Recent entries to
  respect: **Step**, **Backend**, **Trigger**, **Transition**, **Sessions screen**, **Session screen**,
  **Composer**, **Transcript**, **Delta kind**, **Message id**, **Turn boundary**. `Status` in code is
  **State** in the glossary, and a delta's node is a "kind's node", never a "channel"; a second
  spelling is forbidden, including inside an entry's own prose.

### Reviews

- **Reviews are saved verbatim** and a `## Resolution` section is appended afterwards. Do not edit the
  review body; do not delete a dismissal.
- **The reviewer has no shell.** Its claims must be checked; it says so itself. Expect it to be right
  about structure and to need you to run the gates.
- It reliably finds one real should-fix per story — dead doc links, an inaccurate glossary rule, a
  guard that is not proved non-vacuous, an uncaught `crypto.randomUUID`. Take them seriously.

### Demos

- **Demo commands must be bounded.** Background a server with every fd redirected and `timeout`, and
  kill it by its own pid (write `$!` to a file). A backgrounded server keeps the tool call's stdout open
  and hangs the child. `pgrep -f 'holosmith serve'` matches unrelated processes.
- **`scripts/verify.sh`'s `fetch` waits for EOF**, which an SSE stream never sends. Use `fetch_stream`
  (header block) or `fetch_sse_until` (body lines until a needle, bounded).
- **A create's state is not stable in a demo**: the provisioner runs 200 ms/step under
  `--backend stub`, so a `GET /api/sessions` a second later shows `idle`, not `starting`. Assert what
  the answer *is*, not what it was.

---

## 7. Decisions taken on the operator's behalf (do not silently undo)

| Decision | Why | Where recorded |
| --- | --- | --- |
| Default branch stays `master` | the repo's own default; renaming would be an invented change | commit history, every run note |
| Ship locally, never fake a push | no credentials exist (`Permission denied (publickey)`, re-tested); reporting the blocker is the honest path | every issue's run note |
| Rust + root Cargo workspace, one binary crate | stories assume Rust; ADR-0004 | ADR-0004 |
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
| The cap refuses with 409 rather than evicting | nothing is evictable until HS-019 can stop and HS-020 can evict | ADR-0012, HS-020 refines |
| The mutex stays and the writer task is added on top | ADR-0012's "HS-009 replaces it" conflated a lock with a task | ADR-0013 |
| The SSE heartbeat is axum's `KeepAlive`; `futures-core` is a direct dependency | writing our own comment line would double the heartbeat; the `Stream` trait must be nameable and the crate adds no package | ADR-0014 |
| One backend trait with **one method per provisioning step** plus `undo`; an enum `Backends`, not `dyn` | the story's progress reporting *is* step granularity; `async fn` traits are not object-safe and both impls are in-crate | ADR-0015 |
| The stub's per-step delay is a constructor argument (200 ms under `serve`), not a config key | a stub's pacing is not a machine setting; ADR-0006's table is settings | ADR-0015 |
| `backend` **is** a config key (default `docker`), with `--backend` overriding it and the override written back into the loaded config | the story names both, and ADR-0006 says an override is applied to the loaded value | ADR-0015 |
| The transition table is data keyed by `(from, trigger)`; a caller never names a target; one function applies it | a story cannot invent an illegal state, and the write and its event cannot diverge | ADR-0016 |
| "Impossible outside it" is a source walk over `src/**`, not a `pub(crate)` write | the repository's own tests live outside the crate and are HS-007's coverage of the write | ADR-0016 |
| The recovery row admits the three **live** states, not `any` | the spec's cell is "server crash", not a state; `stopped` would clear `stopped_at` and restart retention | ADR-0016 |
| `GET /api/sessions` answers `{"sessions":[…],"network_default":…}`; one `EventSource` per visible live session; the page holds only the DOM | one round trip for the form's default; the cap bounds open streams; nothing to reconcile after a reconnect | ADR-0017 |
| The transcript's four event kinds are pinned before their producer exists; a block is keyed by message id; a delta is one appended text node and a snapshot the only replacement | HS-015 must emit one shape, not choose its own; re-reading the accumulated text per delta is O(n²) per turn; a snapshot arriving after partial deltas must replace, not append | ADR-0018 |
| The renderer's rules live in `web/src/transcript.ts` and are tested with node's own runner, wired into `build-web.sh` | there is no browser here and no producer until HS-015; node 24 strips types natively, so the test costs no dependency and no lock change | ADR-0018 |
| Story docs in `docs/stories/` are not edited — they are the source the Linear issues were imported from | editing them drifts from Linear; the ADR carries the contract instead | PER-34 plan comment |
| This run implements **S/M stories in-session** and delegates **M/L with new surface** to a worker child | the orchestrator already holds the plan for small ones; a fresh context is better for a large UI | §5, and every Linear note since HS-011 |

---

## 8. Resume checklist

```bash
cd /home/adamolsson/projects/holosmith
git status --short                          # must be clean
. ~/.pi/toolchains/env.sh
python3 /tmp/lin.py issues | grep -v ' Done ' | head   # next unfinished issues (recreate /tmp/lin.py if missing)
python3 /tmp/lin.py issue PER-47            # sanity-check Linear is reachable and scoped
scripts/verify.sh                           # baseline green (~3 min, 23 cases, 82 assertions)
```

Then start the cycle at **`PER-47` (HS-015 — prompt and the turn lifecycle)**. Read §1's two facts
about it first: nothing emits a message yet, and the session screen it renders into has never been
looked at in a browser.

### What HS-015 is about, so the first brief is fast

Acceptance, verbatim from `docs/stories/phase-1-session-lifecycle-stub.md:155`:

- `POST /api/sessions/:id/prompt` returns 202, moves `idle → running`, and rejects with 409 unless the
  session is `idle`.
- `agent_settled` moves `running → idle` and is the only normal way out of `running`.
- The stub's scripted turn includes thinking, text, a tool call and usage, so the UI exercises every
  rendering path.
- Turn start and end are explicit events, so the transcript has visible boundaries.
- `last_active_at` updates on prompt, on settle and on steer (drives HS-020 and HS-021).

What is already in place, so the brief can skip it: the transition table's `prompt` and
`agent_settled` triggers (`sessions::state`, ADR-0016) — the prompt handler decides *whether* to apply
one, never a target state; the four pinned event kinds (ADR-0018); `SessionBackend`'s step methods and
the stub's scripted turn slot (`backend/stub.rs`); `Transitioner` to move the row; and the session
screen, which already renders exactly these frames — the composer's submit handler is the one place
that needs the real request.

Things to decide canonically when planning it, and record: how the stub's script is parameterised (a
constructor argument, as ADR-0015 did for step pacing — not a config key), what `message_id` values the
script mints, what `turn.end`'s payload carries (ADR-0018 reads none, so it is free), how a tool call
reaches the transcript when `tool.*` is HS-033's and the screen drops unknown kinds today, whether
`usage` rides on `message.end` as item 15 says, and how the 409 refusals get their codes
(`session_running`? the code table in `http/error.rs` is the only place a code is spelled).

---

## 9. Reporting

Keep a running ledger in the conversation (issue → plan → implement → review → fix → ship, with SHAs) so
a restart can see what was done from Linear, `docs/review/` and `git log`. The Linear issue is the
durable record; the review file is the durable review; the commit history is the durable change. If you
stop before the backlog is empty, say so explicitly, say which issue is next, and leave the tree clean
and the gates green — a half-finished story is worse than an honest stop.
