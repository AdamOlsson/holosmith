# RESUME — orchestrator handoff

You are the **orchestrator for implementing Holosmith**. Read `PROMPT.md` first: it is the standing
operating procedure and it still governs. This file is the delta — the state of the world, the
environment repairs already made, the decisions taken on the operator's behalf, and the landmines that
cost time to find. Read it all before touching anything.

The run is **fully automatic**. The operator is not present. Never ask them a question; decide
canonically, record the decision in the Linear issue and the commit message, and continue.

---

## 1. Where the work stands

Branch `master` (this repo has **no `main`**; do not rename it). Latest commits:

| Commit | What |
| --- | --- |
| `d302635` | HS-001 — workspace layout, static-artifact ADRs (ADR-0004, 0005) |
| `bbb1f0f`, `a45edac` | HS-002 — one typed config, ADR-0006 |
| `50c20ee`, `e71da58`, `cc9aadb` | HS-003 — `serve`, embedded web app, ADR-0007, 0008 |
| `a4774b9`, `bd3b73e`, `ae92a7d` | HS-004 — SQLite, embedded forward-only migrations, ADR-0009 |
| `6c4f02c`, `6994c0f` | HS-005 — browser-facing guards, ADR-0010 |
| `2af15d4`, `108a6ee` | HS-006 — `scripts/verify.sh`, the one-command gate |
| `ad3ffc0`, `fad974d` | HS-007 — session row + repository, ADR-0011, schema 2 |
| `c5891e4`, `858d295` | HS-008 — `POST /api/sessions`, ADR-0012 |
| `5cae107`, `dae1fd9` | HS-009 — event log + single writer, ADR-0013, schema 3 |
| `3b03c5c`, `07f318d` | HS-010 — SSE stream, `Last-Event-ID`, ADR-0014 |

**Linear state: 10 Done, 70 Backlog, all in project `Holosmith`** (`P-PER-2`, team
`Personal`/`PER`). Done: HS-001…HS-010 (`PER-33`…`PER-42`).

**Next issue: PER-43 (HS-011 — stub backend and provisioner steps).** Unblocked: HS-009 and HS-010
both landed. Then PER-44 (HS-012 state machine), PER-45 (HS-013 UI), and so on in `HS-###` order.

Ranking, per `PROMPT.md`: Linear priority (all are `No priority`) → `HS-###` order in
`docs/stories/` → anything already `In Progress` first → skip an issue whose `Blocked by` stories are
not `Done`.

---

## 2. Environment (already repaired — do not redo)

Everything below was installed by the previous orchestrator **outside the repo**. It survives, but if
this is a fresh container, redo it exactly as written.

```bash
. ~/.pi/toolchains/env.sh      # ALWAYS source this before cargo/rustc
```

That file sets `RUSTUP_HOME`, `CARGO_HOME` and `PATH` to:

- rustup 1.99.1, minimal profile + clippy/rustfmt, targets `x86_64-unknown-linux-musl` and
  `aarch64-unknown-linux-musl`;
- musl.cc cross toolchains for both arches (`x86_64-linux-musl-gcc`, `aarch64-linux-musl-gcc`);
- `~/.pi/toolchains/bin/cc` → `/usr/bin/gcc` (the `cc` crate needs a `cc` on PATH);
- `~/.pi/toolchains/bin/qemu-aarch64-static` (7.2.0) — an **optional dev tool** so the aarch64 artifact
  can actually be *run* here. No repo script may download it; `scripts/check-static.sh` prints one
  `SKIP` line when it is absent and still passes.

Why outside the repo: `/` is a 3.4 GB tmpfs and `~/.pi` is the only large writable ext4 mount.

**Not available, and not to be invented:** no Docker daemon, no `sudo`, no `apt`, no `gh`, no ssh key
or token — **pushing is impossible**. Ship as local commits on `master` and report the blocked push.
node 24.22.2 / npm 11.19.1 are available and the npm registry is reachable.

### Linear access

`list_issues`/`save_issue`/`save_comment` are reachable through the same HTTP MCP endpoint that
`scripts/import-linear.py` uses. There are no Linear tools in the tool list, so drive it from a small
wrapper. Recreate `/tmp/lin.py` if it is gone:

```python
#!/usr/bin/env python3
# Thin CLI over the Linear MCP server, reusing scripts/import-linear.py plumbing.
#   python3 /tmp/lin.py issues            # id, title, status
#   python3 /tmp/lin.py issue PER-37      # full issue JSON
#   python3 /tmp/lin.py comment PER-37 FILE
#   python3 /tmp/lin.py state PER-37 "In Progress"
#   python3 /tmp/lin.py states            # the team's states
#   python3 /tmp/lin.py call get_issue '{"id":"PER-37"}'
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

**Token expiry is routine, not a blocker.** When a call prints
`The Linear access token has expired. Re-authenticate 'linear' in pi.`, refresh it in place — the
store keeps a refresh token and a client id:

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

If that fails, **stop and report** — that is the `PROMPT.md` stop condition.

---

## 3. The repository as it stands

```
Cargo.toml                  workspace: member crates/holosmith, rust 1.99, edition 2024, one release profile
Cargo.lock                  COMMITTED, registry-only (no git+ sources — check-static.sh fails on one)
.cargo/config.toml          musl linkers + relocation-model=static (non-PIE) for both targets
pins/pi.txt                 THE ONLY copy of the pinned pi version. Never repeat the literal anywhere.
crates/holosmith/
  build.rs                  git tag (env → git describe → unknown), HOLOSMITH_TARGET, rerun-if-changed for
                            pins/pi.txt, migrations/ and build_support/
  build_support/web_embed.rs  embed-vs-placeholder decision + placeholder marker
  migrations/0001_initial.sql creates schema_version
  src/main.rs               dispatcher only
  src/cli.rs                clap: version, config {validate,show,edit}, serve
  src/commands/             version.rs, config.rs, serve.rs
  src/config/               mod.rs (typed Config, 25 keys, validation), schema.rs (KeySpec table + walk),
                            problem.rs, render.rs, duration.rs, editor.rs
  src/http/                 mod.rs, app.rs (Routes/App seam, is_guard_exempt), error.rs (envelope), health.rs
  src/db.rs                 pragmas + read-back, apply_pending, busy mapping
  src/sql.rs                the one SQL statement scanner (shared by the runner and the _contract_ guard)
  src/schema.rs             SCHEMA_VERSION literal, include_dir! embed, check_embed, contract_violations
  src/{serve,web,runtime,logging}.rs
  tests/                    version_cli, config_cli, http_contract, serve_cli, db_migrations, pin_single_source
web/                        Vite 7 + TypeScript single page; package-lock COMMITTED; dist/ and node_modules/ ignored
scripts/build-web.sh        npm ci + npm run build, asserts web/dist/index.html
scripts/build.sh            web first, HOLOSMITH_REQUIRE_WEB=1, both musl targets → dist/<triple>/holosmith,
                            atomic copy, then check-static.sh
scripts/check-static.sh     file/readelf, empty-root serve + cmp against web/dist/index.html (native and via
                            qemu), marker gate, app.db assertions, byte grep for the schema SQL, Cargo.lock guard
scripts/check-embed.sh      the three embed cases (present / absent warns / required fails)
docs/adr/0001…0009          read 0004–0009 before planning anything
docs/build.md               prerequisites, gates, reproducibility, migrations, placeholders
docs/review/PER-33,34,35,36 each a verbatim review + a `## Resolution` table
docs/stories/               the 80 story slices — the dependency order and definition of done
```

**Gates for this repo** (run them yourself before every commit; a story is not done until they are green
on a clean tree):

```bash
. ~/.pi/toolchains/env.sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
scripts/build.sh            # includes scripts/check-static.sh
```

Then run the story's own demo yourself. `scripts/verify.sh` is the one-command gate: `fmt`,
`clippy`, `test`, `build` (which includes `scripts/check-static.sh`), `embed`, then the shipped
artifact booted with its cases. It exists since HS-006; every later story adds at least one
`case_*` to it (docs/build.md).

---

## 4. The per-issue cycle (what actually worked)

For each issue, in this order. Everything is sequential: **one writer at a time**, never two children
editing files.

1. **Pick** the issue (ranking above) and read its Linear body plus the matching `docs/stories/` slice.
   Note the HEAD SHA — that is the review's fixed point.
2. **Plan** with a fresh `delegate` child, briefed to run the grilling protocol
   (`~/.agents/skills/grill-with-docs/SKILL.md` → `grilling` + `domain-modeling`) and to put one round of
   frontier questions to **you** via `contact_supervisor`. Answer with
   `subagent_supervisor({action:"reply", replyTo, message})`. In your brief: state the environment facts,
   name the files to read, list the decisions already settled (so it does not re-litigate), and **pre-answer
   the obvious round-2 questions** — that is what kept these stories to 1–2 rounds. It may write only
   `GLOSSARY.md` and `docs/adr/*.md`.
3. **Record in Linear before any code exists**: `save_comment` with the plan shape, a decisions→precedent
   table, acceptance restated as testable checks, non-goals, docs touched, the demo, and handoffs; then
   `save_issue` → `In Progress`. Keep the comment self-contained but not enormous — later readers are the
   operator and the next orchestrator.
4. **Implement** with a fresh `worker` child, pointed at the saved plan artifact path. Tell it: do not
   re-litigate, do not commit/push/Linear, do not touch off-limits files, **run the full gate set and the
   demo**, and — critically — **budget discipline**: keep the tree compiling, and if it runs long, stop green
   and report which steps remain. A timed-out child that leaves a broken tree costs more than a partial one.
5. **Verify yourself.** Never trust the child's pasted output: run the gates and the demo. This has caught
   real problems (see §6).
6. **Review** with a fresh `reviewer` child running `~/.agents/skills/code-review/SKILL.md`, read-only,
   against `<fixed-point>...HEAD` plus untracked files, with the spec axis sourced from the Linear body +
   story slice + spec items. Save its message **verbatim** as `docs/review/<IDENT>.md`. Note: this reviewer
   role has **no shell**, so it reasons from code — its claims must be checked, and it says so itself.
7. **Fix** with a fresh `worker` child given `docs/review/<IDENT>.md`: triage every finding as fixed or
   dismissed-with-citation, prove each new guard fires, re-run the gates. Then **you** append a
   `## Resolution` table to the review file (finding → fixed/dismissed → how/why) and commit it with the
   fixes.
8. **Ship**: focused conventional commits naming the issue, the last one ending `Closes <IDENT>`. Rebuild
   after committing — `git describe` changes, and `check-static.sh` now fails on a stale `dist/`. Then
   `save_comment` with what shipped, gates, verbatim demo output, acceptance item by item, and follow-ups;
   `save_issue` → `Done`; re-read the issue to confirm.

Commit messages are prose about *why*, not a restatement of the diff. Look at the existing nine for tone.

---

## 5. Landmines (each of these cost real time)

- **The pinned pi version lives in exactly one tracked file, `pins/pi.txt`.** `tests/pin_single_source.rs`
  fails if the literal appears in a second tracked file — including `docs/review/*.md`, a commit message is
  fine, a doc file is not. Every saved review and Resolution table in this repo deliberately never spells it
  out; keep it that way, and never paste a `version` output into a tracked file.
- **Never export `RUSTFLAGS`.** It silently disables the per-target `rustflags` in `.cargo/config.toml` and
  ships static-PIE. `check-static.sh` asserts the resulting property so a mistake fails loudly.
- **A bare `cargo build` without `web/dist` succeeds with a placeholder page.** `scripts/build.sh` sets
  `HOLOSMITH_REQUIRE_WEB=1`, and `check-static.sh` greps the artifact for the marker. Build the web app
  first (`scripts/build-web.sh`) or your demo will show the placeholder.
- **Test isolation.** Three `serve` tests once created and migrated the *real* `/var/lib/holosmith` and the
  gates passed, because this sandbox lets an unprivileged user write `/var/lib`. Every test that starts
  `serve` must give it a temp `data_root`. Proving a test's isolation means running it and checking the real
  path is untouched.
- **`git describe` and `dist/`.** After a commit, `dist/` is stale and `check-static.sh` fails by design.
  Rebuild after committing, before the demo you paste into Linear.
- **Demo commands must be bounded.** Background a server with every fd redirected and `timeout`, and kill it
  by its unique temp data dir. A backgrounded server keeps the tool call's stdout open and hangs the child;
  `pgrep -f 'holosmith serve'` matches unrelated processes.
- **Same-process SQLite contention bypasses the busy handler.** A two-thread concurrency test is flaky by
  construction; test the lock with a second *process*.
- **Banner line order is pinned by tests** (`control_listen` sixth-ish, `control_origin`, `preview_listen`,
  then `database … schema n`). Change the banner and the test together, and say so in the ADR.
- **The `Routes`/`App` seam is load-bearing**: `Routes` must have no constructor from a bare `Router` and no
  accessor yielding one, and `axum::serve` appears once. HS-005 builds the guard layer inside `App::new`;
  do not let it add a second assembly point.
- **Errors are one envelope**, `{"code","message"}` (ADR-0008); startup failures are
  `holosmith: <command>: <what failed>: <why>` on stderr with exit 1. Clap owns exit 2.
- **Config output** uses `# <key> unset` rather than omitting an unset optional, and `config show` output must
  re-validate clean. `config edit` never removes a directory, never reverts the operator's save.
- **`docs/build.md` is a story deliverable, not orchestrator-only.** One story shipped a stale build doc
  because the brief said off-limits; the reviewer caught it. If a story changes prerequisites or gates, the
  doc is in scope.
- **Reviews are saved verbatim** and a `## Resolution` section is appended afterwards. Do not edit the review
  body; do not delete a dismissal.

- **Every fallible axum extractor answers `text/plain`, not the envelope.** `axum::Json<T>`'s
  rejection and the `Bytes` extractor's rejection (`UnknownBodyError` 400, `LengthLimitError` 413) all
  bypass ADR-0008. Take the raw body and parse it yourself, and take extractors as
  `Result<T, Rejection>` and map them through `ApiError`. Both traps were found the hard way (HS-008).
- **`scripts/build.sh` builds with `cargo build --release --locked`.** Adding a dependency without
  regenerating `Cargo.lock` fails the whole gate. Regenerate the lock, check its diff shows only the
  edge you meant, then run the gate (HS-010).
- **The two SQL-location guards share one helper in `src/sql.rs`** and name their scratch directory
  from their own table word. They once shared a label, and the sessions guard failed intermittently
  because the events guard `remove_dir_all`'d its directory — a real flake, not a theory (HS-009).
- **A seq counter must never be derived from `MAX(seq)`.** Compaction deletes event rows, so the
  counter is the durable `sessions.last_seq`, bumped inside the same transaction as the insert.
- **The writer has one broadcast channel for all sessions**, so an SSE forward loop must filter by
  `session_id` before comparing seqs — otherwise one session's frames reach another's response and a
  foreign seq corrupts the resume point (HS-010's blocker).
- **`scripts/verify.sh`'s `fetch` waits for EOF**, which an SSE stream never sends; use `fetch_stream`
  with its own bound. And **every story adds at least one `case_*`** — the reviewer enforces the rule
  written in `docs/build.md`, and HS-007 was caught breaking it one story after it was written.
- **Adding an ADR means updating `docs/stories/README.md`**: the "Decisions taken while slicing" count
  and the bullet list. Earlier stories did this; a reviewer will notice if you do not.
- **`scripts/check-static.sh`'s banner wait is a fixed 200 × 50 ms (10 s)** and has flaked once under
  load average ≈4 on the aarch64 qemu path. Pre-existing; widen the bound if it recurs (the loop exits
  as soon as the banner appears, so a larger bound costs nothing).

---

## 6. Decisions taken on the operator's behalf (do not silently undo)

| Decision | Why | Where recorded |
|---|---|---|
| Default branch stays `master` | the repo's own default; renaming would be an invented change | commit history, Linear run notes |
| Ship locally, never fake a push | no credentials exist; reporting the blocker is the honest path | every issue's run note |
| Rust + root Cargo workspace, one binary crate | stories assume Rust; ADR-0004 | ADR-0004 |
| Static non-PIE musl on both arches, proven with bwrap + qemu, container runs deferred to HS-046 | no Docker daemon; ADR-0003 says a check belongs to the story where changing the design is still cheap | ADR-0005 |
| Config: one typed value, `KeySpec` single table, three-pass validation, `# <key> unset` | "reports every problem at once" is impossible with a single `toml` parse | ADR-0006 |
| axum + the `Routes`/`App` seam, placeholder+marker for the web app, shutdown deadline with streams cut | measured: `axum::serve` rejects a tower-wrapped service; an endless stream blocked shutdown 29 s | ADR-0007 |
| One JSON error envelope | the spec names codes, never bodies | ADR-0008 |
| SQLite: pragmas read back before any transaction, one transaction per start, SQLite's own write lock, `_contract_` naming, older-binary warns | measured: WAL inside a transaction is silently refused; only prepare-time errors carry SQL text | ADR-0009 |
| Story docs in `docs/stories/` are not edited — they are the source the Linear issues were imported from | editing them drifts from Linear; the ADR carries the contract instead | PER-34 plan comment |

---

## 7. Resume checklist

```bash
cd /home/adamolsson/projects/holosmith
git status --short                 # must be clean
. ~/.pi/toolchains/env.sh
python3 /tmp/lin.py issues | grep -v Done | head    # next unfininished issues  (recreate /tmp/lin.py if missing)
python3 /tmp/lin.py issue PER-43                    # sanity-check Linear is reachable and scoped
cargo test                                          # baseline green
```

Then start the cycle at **PER-43 (HS-011)**. The guard layer HS-005 built is in place, `POST
/api/sessions` (HS-008), the event writer (HS-009) and the SSE stream (HS-010) are live, so HS-011
adds the first real event producers and the stub provisioner. Check `docs/adr/0013` and `0014` for
the writer and stream contracts before briefing it.
