# RESUME — orchestrator handoff (cold start)

You are the **orchestrator for implementing Holosmith**. Read `PROMPT.md` first: it is the standing
operating procedure and it still governs. This file is the delta — the state of the world, the
environment repairs already made, the decisions taken on the operator's behalf, and the landmines that
cost real time. Read it all before touching anything.

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

Branch **`master`** (this repo has **no `main`**; do not rename it). The tree is clean. The last
story commit was **`bf0f460`** (a docs commit bringing the previous handoff up to date); any commit
after it is this file's own bookkeeping.

**Linear: 13 Done, 67 Backlog, all in project `Holosmith`.** Done is `HS-001` … `HS-013`
(`PER-33` … `PER-45`).

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
| HS-011 stub backend + provisioner | PER-43 | `9071390`, `b5022fc` | 0015 |
| HS-012 one state machine | PER-44 | `9424677` | 0016 |
| HS-013 sessions list + create form | PER-45 | `47471c4` | 0017 |

**Next issue: `PER-46` (HS-014 — UI: session screen with a live transcript).** Unblocked: HS-013
landed. Note what HS-014 needs that no story has built yet: there is **no `GET /api/sessions/:id`**,
and **no message producer** — HS-015 owns the stub's scripted turn, so HS-014 can render deltas,
snapshots, turn boundaries and the state badge only against events it cannot yet receive, and its
"prompt a session" demo cannot run until HS-015 wires the composer. Then `PER-47` (HS-015 — prompt
and the turn lifecycle), `PER-48` (HS-016 — abort), and onward in `HS-###` order.

Ranking, per `PROMPT.md`: Linear priority (all are `No priority`) → `HS-###` order in `docs/stories/`
→ anything already `In Progress` first → skip an issue whose `Blocked by` stories are not `Done`.

**Pushing is impossible.** `origin` is `git@github.com:AdamOlsson/holosmith.git` and it answers
`Permission denied (publickey)` (re-tested after HS-013): there is no ssh key and no `gh`. Every story
so far is local commits on `master` and a closing comment that says so. Do not fake a push; report the
blocker.

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
`grep`, `tr`, `cut`, `cmp`, `find`, `timeout`.
**Not available, and not to be invented:** no Docker daemon, no `sudo`, no `apt`, no `gh`, no ssh key or
token. **No `awk`** and **no `shellcheck`** on this machine — do not write a script or a gate that needs
either.

### Linear access

`list_issues` / `save_issue` / `save_comment` / `get_issue` are reachable through the same HTTP MCP
endpoint `scripts/import-linear.py` uses. There are no Linear tools in the tool list, so drive it from a
wrapper. Recreate `/tmp/lin.py` if it is gone:

```python
#!/usr/bin/env python3
# Thin CLI over the Linear MCP server, reusing scripts/import-linear.py plumbing.
#   python3 /tmp/lin.py issues            # id, title, status
#   python3 /tmp/lin.py issue PER-43      # full issue JSON
#   python3 /tmp/lin.py comment PER-43 FILE
#   python3 /tmp/lin.py state PER-43 "In Progress"
#   python3 /tmp/lin.py states            # the team's states
#   python3 /tmp/lin.py call get_issue '{"id":"PER-43"}'
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
leave an untracked `scripts/__pycache__/` behind.

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
cargo test
scripts/verify.sh          # THE one-command gate since HS-006
```

`scripts/verify.sh` runs six phases, cheapest first: `fmt`, `clippy`, `test`, `build`
(`scripts/build.sh`, which itself runs `build-web.sh` first and `check-static.sh` last), `embed`
(`scripts/check-embed.sh`), then `serve` — the **shipped musl artifact** booted on `--port 0` with a
temp data dir, running **17 cases plus a CORS sweep (51 assertions)**, then torn down with `SIGTERM` and
an asserted exit 0. It takes ~70 s and it is the gate that matters. `--no-build` skips `build` and
`embed`; `--keep` keeps the work dir.

A story is not done until all four are green on a clean tree, and then you run the story's own demo
yourself and paste its output.

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
  src/cli.rs                clap: version, config {validate,show,edit}, serve [--backend]
  src/commands/             version.rs, config.rs, serve.rs
  src/config/               mod.rs (typed Config, KeySpec table + walk), schema.rs, problem.rs,
                            render.rs, duration.rs, editor.rs
  src/http/                 mod.rs, app.rs (Routes/App seam, AppState, edge layer, is_guard_exempt),
                            error.rs (the one envelope + every code), health.rs, guard.rs (the four
                            checks), sessions.rs (POST /api/sessions), streams.rs (the SSE handler)
  src/sessions/             mod.rs (Status, Session, NewSession, RepoError),
                            repository.rs (every `sessions` statement + its SQL-location guard)
  src/backend/              mod.rs (Step, SessionBackend, Backends, BackendError), stub.rs, docker.rs
  src/provisioner.rs        the four-step background task and its five `session.status` events
  src/sessions/state.rs     the transition table, `SessionStatus`, `Transitioner::apply`, the guard
  src/events/               mod.rs (Event, NewEvent, EventWriter, EventStream, WriterOptions),
                            writer.rs (the writer thread, group commit, broadcast),
                            repository.rs (every `events` statement + its guard),
                            stream.rs (replay→live, lag handling), registry.rs (open-stream counts)
  src/db.rs                 pragmas + read-back, apply_pending, Database::sessions()/events()
  src/sql.rs                the one SQL statement scanner, and the shared test-only guard helper
  src/schema.rs             SCHEMA_VERSION (= 3), include_dir! embed, check_embed, contract_violations
  src/{serve,web,runtime,logging,clock,version,pins}.rs
  tests/                    version_cli, config_cli, http_contract, serve_cli, db_migrations,
                            pin_single_source, guards, session_repository, sessions_api, events,
                            streams, support/mod.rs (shared harness)
web/                        Vite 7 + TypeScript single page; package-lock COMMITTED; dist/ ignored
scripts/build-web.sh        npm ci + npm run build, asserts web/dist/index.html
scripts/build.sh            web first, HOLOSMITH_REQUIRE_WEB=1, both musl targets → dist/<triple>/,
                            atomic copy, then check-static.sh. Uses `cargo build --locked`.
scripts/check-static.sh     file/readelf, empty-root serve + cmp against web/dist/index.html, marker
                            gate, app.db assertions, byte grep for the schema SQL, Cargo.lock guard
scripts/check-embed.sh      the three embed cases (present / absent warns / required fails)
scripts/verify.sh           the one-command gate (see §3)
docs/adr/0001…0017          read the recent ones before planning anything
docs/build.md               prerequisites, gates, verify.sh, guards, migrations, placeholders
docs/review/PER-33…PER-42   each a verbatim review + a `## Resolution` table
docs/stories/               the 80 story slices — the dependency order and definition of done
GLOSSARY.md                 the domain vocabulary. Check it BEFORE inventing a term.
```

### What exists and what does not

- **Working:** `version` (text + `--json`), `config {validate,show,edit}`, `serve` (control listener,
  embedded app, `/api/health`, `POST /api/sessions`, `GET /api/sessions`, `GET /api/sessions/:id/events`),
  SQLite with three migrations, the guards, the event writer, the SSE stream, the stub backend and its
  provisioner, the state machine, and the Sessions screen with its create form.
- **Not built yet:** `GET /api/sessions/:id`, the session screen (HS-014), any turn (HS-015+), delete,
  resume/prompt/steer/abort endpoints, the preview listener (HS-041), Docker (HS-024+), merge, publish,
  push, the hub, releases.
- **`runtime::Backend`** has `Stub` and `Docker`, `serve --backend` and the config's `backend` select
  it, and `backend::SessionBackend` is what a backend must do. `DockerBackend` refuses every
  operation until HS-024, so a default build provisions to `error` with a reason rather than hanging
  in `starting`.
- **`AppState`** is `{ config, runtime, database: Arc<std::sync::Mutex<Database>>, events: EventWriter,
  streams: StreamRegistry }`. The mutex is an explicit stopgap (ADR-0012/0013) — see §6.

---

## 5. The per-issue cycle (what actually worked, ten times)

Everything is **sequential: one writer at a time**, never two children editing files.

1. **Pick** the issue (ranking above) and read its Linear body plus the matching `docs/stories/` slice.
   Note the HEAD SHA — that is the review's fixed point.
2. **Plan.** For **M/L** stories, use a fresh `delegate` child briefed to run the grilling protocol
   (`~/.agents/skills/grilling/SKILL.md` + `domain-modeling`), which puts **one** round of frontier
   questions to *you* via `contact_supervisor`; answer with
   `subagent_supervisor({action:"reply", replyTo, message})`. For **S** stories, write the plan
   yourself — it is faster and the analysis is usually already done. In either case: state the
   environment facts, name the files to read, list the decisions already settled so they are not
   re-litigated, and **pre-answer the obvious round-2 questions** — that is what kept these stories to
   one round. The planner may write only `GLOSSARY.md` and a new `docs/adr/*.md`.
3. **Record in Linear before any code exists**: `save_comment` with the plan shape, a
   decisions→precedent table, acceptance restated as testable checks, non-goals, docs touched, the
   demo, and handoffs; then set the issue `In Progress`. Keep it self-contained but not enormous —
   later readers are the operator and the next orchestrator.
4. **Implement** with a fresh `worker` child pointed at the saved plan artifact. Tell it: do not
   re-litigate, do not commit/push/Linear, do not touch off-limits files, **run the full gate set and
   the demo**, and — critically — **budget discipline**: keep the tree compiling, and if it runs long,
   stop green and report which steps remain. A timed-out child that leaves a broken tree costs more
   than a partial one.
5. **Verify yourself.** Never trust the child's pasted output: run the gates and the demo. This has
   caught real problems every single time.
6. **Review** with a fresh `reviewer` child running `~/.agents/skills/code-review/SKILL.md`, read-only,
   against `<fixed-point>...HEAD` plus untracked files, with the spec axis sourced from the Linear body
   + story slice + spec items. Write the diff to a file first (`git diff <fp>` plus every untracked
   file behind a `# NEW FILE:` banner) because this role has **no shell** — it reasons from source, so
   its claims must be checked and it says so itself.
7. **Fix** with a fresh `worker` child given the saved review. It triages every finding as fixed or
   dismissed-with-citation, proves each new guard fires (by mutating the code and watching the test go
   red), and re-runs the gates. Then **you** append a `## Resolution` table to the review file
   (finding → fixed/dismissed → how/why) and commit it with the fixes.
8. **Ship**: focused conventional commits naming the issue, the last one ending `Closes <IDENT>`.
   Rebuild after committing — `git describe` changes and `check-static.sh` fails on a stale `dist/`.
   Then `save_comment` with what shipped, the gates, verbatim demo output, acceptance item by item, and
   follow-ups; set the issue `Done`; re-read it to confirm.

Commit messages are prose about *why*, not a restatement of the diff. Look at the last twenty for tone.

Working artifacts (`/tmp` is ephemeral; the durable record is Linear + `docs/review/` + `git log`):

```text
/tmp/holosmith/plans/PER-<n>.md           the plan
/tmp/holosmith/plans/PER-<n>-linear.md    the Linear comment body
/tmp/holosmith/plans/PER-<n>-impl-brief.md
/tmp/holosmith/reviews/PER-<n>-diff.txt   the diff artifact for the reviewer
/tmp/holosmith/reviews/PER-<n>-review-brief.md
/tmp/holosmith/reviews/PER-<n>-review-verbatim.md
/tmp/holosmith/reviews/PER-<n>-fix-brief.md
```

---

## 6. Landmines (each of these cost real time)

### Build and toolchain

- **`scripts/verify.sh` is at 21 cases / 66 assertions** and takes ~3 min including both musl
  builds. A concurrent `scripts/build.sh` in the same tree can make `check-static.sh`'s banner wait
  flake — run one at a time.
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
- **No `awk`, no `shellcheck`.** Scripts must use `sed`/`grep`/`tr`/`cut`/`cmp`/`find`/`timeout`.

### Tests

- **Test isolation.** Three `serve` tests once created and migrated the *real* `/var/lib/holosmith` and
  the gates passed, because this sandbox lets an unprivileged user write `/var/lib`. Every test that
  starts `serve` must give it a temp `data_root` inside its own temp dir.
- **Two `Database::open`s on one path in one process is ADR-0009's measured "database is locked" trap.**
  A shared test harness must open exactly one (`OnceLock`), and a per-test harness must always take its
  own `TempDir`. `tests/support/mod.rs` carries the pid in the directory name so concurrent runs do not
  collide.
- **Same-process SQLite contention bypasses the busy handler.** A two-thread concurrency test is flaky
  by construction; test the lock with a second *process*.
- **A guard test that shares a scratch directory with another guard flakes.** The two SQL-location guards
  share one helper in `src/sql.rs` and name their directory from their own table word. A shared label
  once made the sessions guard fail because the events guard `remove_dir_all`'d its directory.
- **A test that asserts only a status is how a wrong code ships.** HS-005's review found a test that had
  been silently asserting `bad_client` while labelled `bad_origin`. Assert status **and** `code` **and**
  `content-type`, and prove a new guard fires by removing the behaviour and watching the test go red.

### HTTP and the envelope

- **Every fallible axum extractor answers `text/plain`, not the envelope.** `axum::Json<T>`'s rejection
  and the `Bytes` extractor's (`UnknownBodyError` 400, `LengthLimitError` 413) all bypass ADR-0008. Take
  the raw body and parse it yourself, and take extractors as `Result<T, Rejection>` and map them through
  `ApiError`.
- **The `Routes`/`App` seam is load-bearing**: `Routes` has no constructor from a bare `Router` and no
  accessor yielding one, `axum::serve` appears once, and `Router::layer` runs in exactly one place —
  `App::new`. Do not add a second assembly point; `tests/guards.rs` pins the one `.layer(`.
- **A `GET` route cannot be mutating.** `Routes::route` was deleted; `get`/`head` record `ReadOnly` and
  `post`/`put`/`patch`/`delete` record `Mutating`. Use the helpers.
- **Errors are one envelope**, `{"code","message"}` (ADR-0008); codes are constants in `http/error.rs`.
  No test pins a message sentence. A 500's message must be a **fixed sentence** — a rusqlite message can
  carry the database path.
- **`is_guard_exempt` is the only exemption list** (true only for `GET`/`HEAD /api/health`). Never add a
  second one.
- **Banner line order is pinned by tests** (`log`, version line, `config`, `data_root`,
  `control_listen`, `control_origin`, `preview_listen`, `database … schema n`). Change the banner and
  the test together, and say so in the ADR.
- **`--port 0` re-derives `control_origin` from the bound port** after `bind`; a browser's `Origin` is
  compared as text, so this is what makes the guard work in tests and demos.

### Data

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
  files. Bump it in the same commit as the migration. `ALTER TABLE … ADD COLUMN` goes **last** in its
  migration.
- **The repository owns every statement against its table**, enforced by a test that walks `src/**`.

### Config and docs

- **Config output** uses `# <key> unset` rather than omitting an unset optional, and `config show`
  output must re-validate clean. `config edit` never removes a directory, never reverts the operator's
  save.
- **`docs/build.md` is a story deliverable, not orchestrator-only.** If a story changes prerequisites or
  gates, the doc is in scope. One story shipped a stale build doc because the brief said off-limits.
- **Every story adds at least one `case_*` to `scripts/verify.sh`.** The rule is written in bold in
  `docs/build.md`, and the reviewer enforces it — HS-007 broke it one story after it was written.
- **Adding an ADR means updating `docs/stories/README.md`**: the "Decisions taken while slicing" count
  and the bullet list. Earlier stories did this; a reviewer will notice if you do not.
- **`GLOSSARY.md` is the vocabulary authority.** Check it before inventing a term — HS-007's plan said to
  add `Session`/`Status` when both already existed under other names. `Status` in code is **State** in
  the glossary; a separate entry would be a second spelling, which the glossary forbids.
- **Reviews are saved verbatim** and a `## Resolution` section is appended afterwards. Do not edit the
  review body; do not delete a dismissal.
- **`scripts/check-static.sh`'s banner wait is a fixed 200 × 50 ms (10 s)** and has flaked once under
  load average ≈4 on the aarch64 qemu path. Pre-existing; widen the bound if it recurs (the loop exits
  as soon as the banner appears, so a larger bound costs nothing).

### Demos

- **Demo commands must be bounded.** Background a server with every fd redirected and `timeout`, and
  kill it by its unique temp data dir. A backgrounded server keeps the tool call's stdout open and hangs
  the child; `pgrep -f 'holosmith serve'` matches unrelated processes (and has matched leftovers from
  previous sessions — kill those by pid, they are stale demo servers).
- **`scripts/verify.sh`'s `fetch` waits for EOF**, which an SSE stream never sends. Use `fetch_stream`
  with its own bound.

---

## 7. Decisions taken on the operator's behalf (do not silently undo)

| Decision | Why | Where recorded |
| --- | --- | --- |
| Default branch stays `master` | the repo's own default; renaming would be an invented change | commit history, every run note |
| Ship locally, never fake a push | no credentials exist; reporting the blocker is the honest path | every issue's run note |
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
| The cap refuses with 409 rather than evicting | nothing is evictable until HS-019 can stop and HS-020 can evict; HS-020 refines the branch | ADR-0012 |
| The mutex stays and the writer task is added on top | ADR-0012's "HS-009 replaces it" conflated a lock with a task; a database actor is deferred and not needed | ADR-0013 |
| The SSE heartbeat is axum's `KeepAlive`; `futures-core` is a direct dependency | writing our own comment line would double the heartbeat; the `Stream` trait must be nameable and the crate adds no package | ADR-0014 |
| Every state change goes through `sessions::state::Transitioner::apply`; `set_status` may be named only in `sessions/repository.rs` and `sessions/state.rs`, enforced by a source walk | one table, one function: a story cannot invent an illegal state, and a change is reported because the function emits the event | ADR-0016 |
| The `session.status` payload is `{from, to, step, reason}`; a provisioning step report is `from == to` with `step` set and is not a table row | one payload shape for the UI, and a step beginning is not a state change | ADR-0016 |
| `GET /api/sessions` answers `{"sessions":[…],"network_default":…}`; the screen opens one `EventSource` per visible live session and holds no state beyond the DOM | one round trip for the form's default, and no cache to reconcile after a reconnect | ADR-0017 |
| Story docs in `docs/stories/` are not edited — they are the source the Linear issues were imported from | editing them drifts from Linear; the ADR carries the contract instead | PER-34 plan comment |

---

## 8. Resume checklist

```bash
cd /home/adamolsson/projects/holosmith
git status --short                          # must be clean
. ~/.pi/toolchains/env.sh
python3 /tmp/lin.py issues | grep -v ' Done ' | head   # next unfinished issues (recreate /tmp/lin.py if missing)
python3 /tmp/lin.py issue PER-43            # sanity-check Linear is reachable and scoped
scripts/verify.sh                           # baseline green (~70 s, 17 cases, 51 assertions)
```

Then start the cycle at **`PER-46` (HS-014 — UI: session screen with a live transcript)**.

### What HS-011 is about, so the first brief is fast

Acceptance, verbatim from `docs/stories/phase-1-session-lifecycle-stub.md:82`:

- A backend trait covers: provision, start turn, steer, abort, stop, resume, delete, snapshot. Stub
  implements it with configurable per-step delays.
- `backend = "stub"` is selectable by config and by `--backend`; stub needs no Docker, git or agent
  binary.
- The provisioner runs in the background and emits `session.status` with `step` set to `worktree`,
  `network`, `container`, `pi`.
- On failure it undoes completed steps and sets `error` with a reason; partial resources are not left
  behind.
- Success ends in `idle`, not `starting`.
- The stub can be told to fail at a named step, and a test does exactly that.

What is already in place for it: `runtime::Backend { Stub, Docker }` and `serve --backend`;
`EventWriter::append` (bounded queue, so a producer waits rather than the writer); `SessionRepository`
(`create`, `get`, `by_client_key`, `list`, `set_status`, `live_count`); `Status` with all six values;
`AppState.events`. The writer's commit-before-broadcast contract is ADR-0013; the stream's replay
contract is ADR-0014. **Read both before briefing.**

Things to decide canonically when planning it, and record: where the provisioner task lives and how it
is spawned (it must not hold the `Mutex` across an `await`); how a failure undoes a step when the stub
creates nothing (it must still *emit* the undo, or HS-024's Docker backend has no shape to fill); the
`session.status` payload shape (`{step, status}` today in the demo — pin it); and that HS-012 owns the
transition rules, so HS-011 must not invent a second state machine.

---

## 9. Reporting

Keep a running ledger in the conversation (issue → plan → implement → review → fix → ship, with SHAs) so
a restart can see what was done from Linear, `docs/review/` and `git log`. The Linear issue is the
durable record; the review file is the durable review; the commit history is the durable change.
