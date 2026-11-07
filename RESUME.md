# RESUME — one issue per session (next: PER-84 / HS-052, the changes view's summary)

You are the **orchestrator for implementing Holosmith**. `PROMPT.md` is the standing operating procedure and
still governs the cycle; this file is the state of the world, and it adds one rule of its own:

**This session picks up exactly one issue — the one named in §1.** Take it through the whole cycle (§5):
plan, Linear, implement, gates, demo, review, fix, ship, **write the state back** into this file for the next
agent (§5, step 10), then **kill every child you started** and stop (§5, step 11). Do not start a second issue
however much budget is left: a fresh session with a fresh file is how this run continues, and one honest stop
beats two half-finished stories.

The operator is not present. **Never ask a question.** Decide canonically — `HolosmithSpec.md` →
`GLOSSARY.md` → `docs/adr/` → `docs/stories/` → the existing code — record the decision in the Linear issue
and the commit message, and continue.

Read in this order, and no further: **this file**; the issue's slice in `docs/stories/`; the spec items §1
names (and the V-rows); the ADR index (one line per decision, `docs/stories/README.md`), then only the ADRs
§1 names; then `docs/notes.md` — the **cross-cutting** section first, then the sections §6 of *this* file
names for this issue. `docs/notes.md` is the durable prose record (the build's shape, the event contract, the
landmines). Anything about a *finished* story is in Linear, `docs/review/PER-<n>.md` and `git log` — not here.

---

## 0. What this project is

`HolosmithSpec.md` is authoritative. A single-operator system that runs coding agents in containers on
machines you own, driven from a phone. The work is **80 stories** (`HS-001` … `HS-080`) in `docs/stories/`,
one Linear issue each, in five phases:

| Phase | Stories | File |
| --- | --- | --- |
| 0 — walking skeleton | HS-001 … HS-006 | `docs/stories/phase-0-walking-skeleton.md` |
| 1 — session lifecycle (stub backend) | HS-007 … HS-023 | `docs/stories/phase-1-session-lifecycle-stub.md` |
| 1b — Docker backend | HS-024 … HS-046 | `docs/stories/phase-1b-docker-backend.md` |
| 2 — deployable MVP | HS-047 … HS-067 | `docs/stories/phase-2-deployable-mvp.md` |
| 3 — multiple machines | HS-068 … HS-074 | `docs/stories/phase-3-multiple-machines.md` |
| 4 — self-development | HS-075 … HS-080 | `docs/stories/phase-4-self-development.md` |

`AGENTS.md` carries the repository rules, including that **Linear scope is exactly one project**: `Holosmith`
(`P-PER-2`, team `Personal`/`PER`). `VulkanRenderAPI` belongs to another repository — never read, create,
comment on, or close anything there.

**Branch `master`** (this repo has **no `main`**; do not rename it). Every commit is pushed; pushing is a
sandbox-specific incantation (§2).

---

## 1. The issue this session picks up: `PER-84` — HS-052, the changes view's summary

**State when this file was written** (the story's last commit is `origin/master` and the tree is clean apart from the
operator's own `log.txt`/`orchestrate.sh`): **56 of 80 stories Done, 24 Backlog**, all in project `Holosmith`.
`PER-84` is `Backlog`, it is the next in `HS-###` order, and its one blocker (`HS-046`, the verification run) is
`Done` — so it is eligible. Nothing in the queue is blocked on the machine (§2).

The last shipped story was `PER-83` (HS-051, `drain` over the admin socket, **ADR-0060**) — the sixth of phase 2's
twenty-one. Its gate run was green: **77 cases, 1101 assertions**, no `SKIP` line; `cargo test` is **919 tests across
51 targets**, and `web/`'s runner is **52**. Its §1 material is now Linear, `docs/review/PER-83.md`, `git log` and §9.

### 1.1 The slice (verbatim from `docs/stories/phase-2-deployable-mvp.md`; Linear's body is the same text)

> ### HS-052 — Changes view: summary
>
> **As** the operator **I want** to see what a session changed and whether it can merge **so that** I can decide
> without opening a terminal.
>
> - **Size:** M · **Blocked by:** HS-046 · **Spec:** item 12
> - **Demo:** `GET /api/sessions/:id/changes` shows branch, ahead/behind, dirty, mergeable, per-file line counts and
>   `touches_ci`.
>
> **Acceptance**
> - [ ] Returns `{branch, base_commit, head, target, ahead, behind, dirty, mergeable, files[], touches_ci}`.
> - [ ] `behind` counts commits on the target the branch lacks.
> - [ ] `mergeable` comes from `git merge-tree --write-tree`, which touches no working tree.
> - [ ] `dirty` reflects uncommitted work in the session's worktree.
> - [ ] All git work runs on the host in the project repository; no container is involved.
> - [ ] A missing or pruned worktree degrades to a clear 409 rather than a 500.

`docs/stories/**` is **not edited** (§7). An **M** story with one new endpoint and one new module: the git reads are
mostly neighbours of things `worktree.rs` already does, so read the code before deciding how much is new.

### 1.2 The spec it closes

**Item 12's first bullet**, in full:

> `GET /api/sessions/:id/changes` returns `{branch, base_commit, head, target, ahead, behind, dirty, mergeable,
> files:[{path, status, additions, deletions}], touches_ci}`. `behind` counts commits on the target that the branch
> lacks. `mergeable` comes from `git merge-tree --write-tree` (git 2.38 or newer), which touches no working tree.
> `dirty` means uncommitted work in the session's worktree.

The rest of item 12 is **not** this story: the diff endpoint is HS-053, `POST /merge` and its five rules are
HS-054–HS-057, `publish` is HS-058, and the Session screen's Changes sheet is the UI story after them. Item 12's
"all git work for review and merge runs on the host in the project repository; containers never merge" is the one
sentence that governs this story as well as the merge — the answer is read in the **project** repository on the host,
never asked of the container.

### 1.3 What already exists

- **`worktree.rs` owns the paths and two of the five git questions.** `Worktree::of(projects_root, project, id)` is the
  naming rule, `BRANCH_PREFIX` is `agent/`, `WORKTREES` is `.worktrees`; `ahead(base_commit)` is already
  `rev-list --count` on the branch against its base (HS-025/043), `registrations` reads `git worktree list --porcelain`
  (HS-044), and `repository_of(projects_root, project)` is the host repository's path. `GitError` is the error type and
  `refuse`/`Refusal` are the project refusals (ADR-0030) — the *create*'s, not a read's.
- **The row carries what a changes reader needs.** `sessions.worktree` and `sessions.base_commit` are written by the
  provisioner's worktree step (`set_worktree`), and `Session` carries them, so a handler has the path and the base
  without recomputing either.
- **`config.merge.targets` already exists** (the `Merge` struct, validated in `config/schema.rs`): default `main` and
  `master`, whichever exist. That is where `target` comes from, and the spec's "the UI preselects the first" is the
  ordering that key already has.
- **The git version gate is already there**: `preflight::MIN_GIT` is 2.38 and `preflight`'s `git` check refuses an
  older one, which is `merge-tree --write-tree`'s own precondition. This machine has git **2.43.0**.
- **The envelope and its codes** (`http/error.rs`, ADR-0008), the `Routes`/`App` seam and `is_guard_exempt`
  (`http/app.rs`), and `refuse_project`/`refusal_error` (`http/sessions.rs`) — the shape every handler and every
  refusal already has.
- **The blocker's trick for a real worktree without a daemon**: `case_worktree` boots `backend = "docker"` against a
  fake `_ping` socket, so the create provisions the **worktree** step (which writes the row) and then fails at the
  container step. That is what a `case_changes` can use to get a worktree on a real repo with no daemon — and
  `tests/worktree.rs` already drives `Worktree` directly with a real git repo in a `TempDir`.
- **`tool.rs`'s 404 family** (`unknown session`, "an id that cannot be a filename", "a missing file") is the pattern
  for a read that says what it could not find rather than 500ing.

### 1.4 What does not exist

No `/changes` route (the `App` has thirteen and none is `changes`), no `behind`, no `dirty` (`status --porcelain`), no
`mergeable` (`merge-tree --write-tree`), no `files[]`/numstat read, no `touches_ci`, no module for any of it, no code
for a missing-or-pruned worktree, no `case_changes`, and no UI (the Changes sheet is not this story's).

### 1.5 The decisions that shape the design

1. **Where the module lives.** A new `src/changes.rs` (a `Changes::of(...)` that composes the five reads and a type
   with the eleven fields) beside `worktree.rs`, rather than five methods on `Worktree`: the diff basis and the JSON
   are the changes view's, not the worktree's, and `worktree.rs` is already the biggest git module. Say so in the ADR.
2. **The diff's basis: `base...head` or `base..head`?** Item 12 says "what a session changed", and `behind` counts
   commits on the *target* the branch lacks — so this story's basis is the worktree's branch against its recorded
   `base_commit`. Whether the file list is `base_commit..head` (two-dot) or `base_commit...head` (three-dot, i.e.
   since the merge base) is a real decision that HS-053's diff and HS-054's mergeability both inherit: pick one,
   record it, and make the field's name say it.
3. **What `head` is.** The branch's tip (`agent/<id>`) or the worktree's `HEAD`? They differ exactly when a turn is
   mid-flight or the worktree is detached, and the answer decides whether `ahead`/`behind` are about the branch or the
   checkout. `ahead` already means the branch.
4. **`target` is one branch, and there may be two.** `merge.targets` defaults to `main` **and** `master`, whichever
   exist: what does `target` say when both exist, or neither? (The spec's "the UI preselects the first" implies the
   first *existing* one; "neither" is a machine this endpoint must still describe.)
5. **`dirty` and untracked files.** `git status --porcelain` counts an untracked file; the WIP snapshot (HS-042) and
   the delete's keep-rules (HS-043) already have an opinion about what counts as work. Decide whether an untracked
   file makes the worktree dirty, and whether an ignored one does.
6. **`mergeable` is the sharpest one: a `bool` over a fallible question.** `merge-tree --write-tree` fails when the
   target does not exist, when the merge conflicts (exit 1 with the conflicted paths), or when git is too old — and a
   `false` for "I could not look" is exactly the P1 HS-050's review found (`Docker::running` folding every error into
   `false`, `docs/notes.md`). The field is a `bool` in the spec's shape, so the honest form is `null` (JSON) with a
   sibling sentence, or `bool` plus the reason — decide, and make `doctor`'s three arms the precedent.
7. **The 409's word.** A missing worktree, a pruned one, a row with no branch and a repo that has gone away are four
   sentences; the acceptance asks for "a clear 409 rather than a 500", so name the code (the family has one per
   refusal) and decide whether they are one code or several.
8. **Where the git runs.** Host, project repository, `spawn_blocking` (the backend's own pattern for `Worktree`), and
   `--no-ext-diff --no-textconv` is HS-053's sentence but this story runs `git diff --numstat`, which can already be
   affected by repository config (`diff.external`, `diff.noprefix`, `.gitattributes` textconv) — decide whether this
   story passes those flags too, because the numbers a client sees come from *this* command.
9. **`touches_ci`'s list.** Item 12's publish rule names `.github/`, "other CI files" and "hook directories". The
   changes view is where that predicate first exists; HS-058 will read it. Decide the set once (a constant), and
   whether an **added** file counts the same as a modified one.
10. **Concurrency.** Nothing here writes, but `merge-tree --write-tree` writes objects, and a session's own worktree
    may be committing at the same moment (`snapshot.rs`, the agent's own git). Is one read per request enough, and
    does it need the per-project start lock the worktree step takes?

### 1.6 The questions the plan must answer (before code)

1. **The exact JSON**, field by field, including the nulls (no target, no worktree, unmergeable-because-unreadable).
2. **Which git command produces each field**, with its flags, and what each returns for a binary file, a rename, an
   empty branch and a branch at its base.
3. **The refusal table**: which condition maps to which code and sentence, and which of them are 409 versus 404.
4. **Where `touches_ci`'s set lives** and how HS-058 will read the same predicate.
5. **Which `case_*` proves each acceptance bullet**, and which half can `SKIP` without skipping on its own subject.

### 1.7 Acceptance, restated as checks

- *Returns the eleven fields* — one `GET /api/sessions/{id}/changes` on a session with a real worktree answers `200`
  with every key the spec names, `files[]` entries carrying `path`, `status`, `additions`, `deletions`.
- *`behind` counts the target's commits the branch lacks* — a target that has moved ahead of the base makes `behind`
  non-zero and `ahead` unchanged; the numbers are asserted against `git rev-list --count` run by the case itself.
- *`mergeable` is `merge-tree --write-tree`* — a conflict planted in the target makes it `false` (with the reason),
  and the case asserts **no working tree moved** (the repo's `git status` and files are unchanged afterwards).
- *`dirty` reflects uncommitted work* — a file written into the worktree flips `dirty` and no other field.
- *All git on the host, in the project repository* — no container is asked: the case runs with **no daemon** (the
  fake `_ping` socket `case_worktree` uses), which is the measurement.
- *A missing or pruned worktree is a 409 with a sentence* — remove the worktree directory (or `git worktree prune`
  it) and assert `409`, a code, and a sentence naming the fix, never a `500`.
- Plus the usual gates, one new `case_*`, and `docs/build.md`/`docs/notes.md`/`GLOSSARY.md`/an ADR (**0061**).

### 1.8 Non-goals

The unified diff (`GET …/changes/diff`, HS-053), `POST …/merge` and its five rules (HS-054–HS-057), `publish`
(HS-058), the Changes sheet (the UI story), any write of any kind, the hub, and any change to `worktree.rs`'s create
refusals.

### 1.9 The demo

On this machine: a project repo under a temp root (a first commit, a branch), a `serve` whose projects root is that
repo, a session created with the fake-`_ping` Docker boot `case_worktree` uses so the **worktree step** runs and the
row gets its path — then commit something on `agent/<id>`, move `main` ahead, write an uncommitted file, and
`GET /api/sessions/{id}/changes` showing `branch`, `base_commit`, `head`, `target`, `ahead`, `behind`, `dirty`,
`mergeable`, `files[]` and `touches_ci`; then a planted conflict for `mergeable: false`, and a pruned worktree for the
`409`. **Say plainly what was not measured**: no real container (the worktree is on the host, which is the story's own
claim), no merge (HS-054), and no UI.

### 1.10 What the next agent needs back

When you write the state back (§5, step 10): §1 becomes the **next** issue — by `PROMPT.md`'s ranking that is `PER-85`
(HS-053, the unified diff) *if* HS-052 ships, and `PER-84` again if it does not — with the same shape. Refresh the
counts, the ADR number you took (**ADR-0061** is next unless HS-052 takes none), the last gate run's own numbers, and
replace §9's row with this story's (short — one paragraph). Anything you learned that is **not** state and **not** an
ADR goes into `docs/notes.md` under a new `### Landmines: …` heading, never into §1. Then kill what you started
(§5, step 11).

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
**Available since HS-024: a real Docker daemon.** `docker version` shows a 29.9.2 server, `/var/run/docker.sock` exists,
`docker run` works and images pull over the network. `bwrap.sh` binds the host's socket at
`/var/run/docker.sock` (`DOCKER=()` … `--bind "$sock" /var/run/docker.sock`), so a `docker` inside the
sandbox drives the real daemon — that is host root by another name and it is granted deliberately; a
machine without a socket simply gets no binds. **`pi-agent:local` is loaded and is the image every Docker
story uses**: its pi version equals `pins/pi.txt` exactly (`agent::version_check` asserts it at launch), git
2.39.5, node 24, default user `node` (uid **1000**, the host uid). Everything the Docker chain needs is here,
including network access for `alpine:3` and friends. **A preview demo needs `node` inside the container** —
this image has it, and so must any image `agent_image` points at (HS-036 learned that the hard way: a missing
node is exit 127, not a test failure). **A `serve` boot starts `hs_egress` and `hs_key` and they outlive
it**, so a gate run or a demo leaves both running: remove them (`docker rm -f hs_egress hs_key`) when you want
`tests/container.rs`'s egress-proxy test to run its *real* path, which it skips while a proxy is running. Since
HS-048 both proxies run `holosmith-proxy:<version>`, built by `server deploy`'s step 3, so a boot on a machine
that has never deployed warns once and starts no proxy — and the gate's `case_server_images` is what leaves that
image behind for every later case.
The machine's own `egress` network must exist for that test (`docker network create egress` is the call a boot
makes) — a cleanup removed it once and the test failed on setup at HS-043's baseline.

**pi's own RPC documentation is on this machine**:
`~/.pi/agent/install/releases/1.5.0/node_modules/@earendil-works/pi-coding-agent/docs/rpc.md` (how to start
it, the JSONL framing rules — one complete JSON object per line, split on LF only, never a reader that also
splits on U+2028), `rpc-commands.md` (the `prompt`/`steer`/`abort`/`get_state` commands and the `disposition`
field), `json.md` (the event stream: `agent_end`, `agent_settled`, tool and message events),
`rpc-extension-ui.md` (the UI requests `pump` auto-cancels) and `environment-variables.md`. The binary in
the image is `/usr/local/bin/pi`, its version already equals `pins/pi.txt`, and `pi --mode rpc --no-session`
is the launch `agent::COMMAND` builds. `pi --help` in the sandbox is the same version's, so an experiment
is free; nothing in the repo may hard-code the path to those documents.

**Not available, and not to be invented:** no `sudo`, no `apt`, no `gh`, no ssh key (the agent has one,
§pushing), **no systemd** (`/proc/1/comm` is `bwrap`; `/run/systemd/system` does not exist) and **no Tailscale
daemon** — the `tailscale` CLI *is* at `/usr/bin/tailscale`, but `tailscale status` answers
`failed to connect to local tailscaled`, which is the fact `doctor` renders as `unknown` rather than failing on.
There is a **browser** (`docs/notes.md`, fact 3), but no *gate* may need one.
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

### Pushing

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

**The reliable form offers ONE key.** The agent holds three, and *which* GitHub accepts changes between
sessions — in this session it was the **third**, offered by `sed -n '3p'`, after the first two were rejected
(`ls-remote` succeeded with it once and failed the next time, so retry the loop rather than trusting one
probe). Loop over the three, offer one at a time, and push as soon as one works:

```bash
for i in 1 2 3; do ssh-add -L | sed -n "${i}p" > /tmp/k.pub
  GIT_SSH_COMMAND="ssh -F /dev/null -o IdentitiesOnly=yes -o IdentityFile=/tmp/k.pub \
    -o IdentityAgent=/run/ssh-agent.sock -o StrictHostKeyChecking=accept-new -o User=git" \
    git push origin master && break; done
```

(The older single-key form, for reference:)

```bash
ssh-add -L | sed -n '2p' > /tmp/pushkey.pub
GIT_SSH_COMMAND='ssh -F /dev/null -o IdentitiesOnly=yes -o IdentityFile=/tmp/pushkey.pub \
  -o IdentityAgent=/run/ssh-agent.sock -o StrictHostKeyChecking=accept-new -o User=git' \
  git push origin master
```

(`-i` names a `.pub` that is in the agent: ssh offers exactly that one and signs with the agent's copy.)
`git ls-remote` with the same options is the cheap way to tell "pushed" from "the agent refused again".
Push after every commit that matters, and if a push fails, report it — do not fake one.

---

## 3. The gates

`. ~/.pi/toolchains/env.sh` first. Then:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                          # lib + every test binary; the run prints the counts
cargo test --test schema_previous   # the `migrations` phase, run on its own
cd web && npm test                  # the transcript's rules, node's own runner
cd web && npx tsc --noEmit          # not a gate phase, but a web edit that does not typecheck is a bug
scripts/verify.sh                   # THE one-command gate since HS-006
```

`scripts/verify.sh` runs seven phases, cheapest first: `fmt`, `clippy`, `test`, **`migrations`**
(`cargo test --test schema_previous` — the expand/contract rule, HS-075), `build` (`scripts/build.sh`, which
itself runs `build-web.sh` — `npm test` then Vite — and `check-static.sh` last), `embed`
(`scripts/check-embed.sh`), then `serve` — the **shipped musl artifact** booted on `--port 0` with a temp data
dir, running its cases plus a CORS sweep, then torn down with `SIGTERM` and an asserted exit 0. **The run
prints its own case and assertion counts — quote those, never a number copied out of this file.** Since HS-046
it also prints one `ok <case>` line per case that measured something: a case that printed `SKIP` withholds that
line and the run ends with `SKIP n of N cases measured nothing`, so `grep -c '^ok case_'` is the number that
proved something and a skip can never read as a pass. It takes
~11–12 min (both musl targets, the Docker cases, `case_idle_stop`'s real minute, `case_preview`'s container
and `case_doctor_full`'s self-test). `--no-build` skips `build`
and `embed`; `--keep` keeps the work dir.

Some cases bring their own boot, and they are the ones a new story is most likely to collide with:

| Case | What it needs |
| --- | --- |
| `case_token_budget`, `case_usage_survives_resume`, `case_compaction`, `case_history`, `case_backup`, `case_worktree`, `case_worktree_refusal`, `case_agent_launch`, `case_docker_resume`, `case_docker_turn`, `case_internet_toggle`, `case_doctor_full` | its own `serve`, and (the last five) the daemon and `agent_image` |
| `case_verification_table` | no server, no daemon, no image: it reads `HolosmithSpec.md`'s eleven `V<n>` rows (and runs first in `serve_cases`, so a rotted table fails before the Docker cases) |
| `case_docker_preflight`, `case_doctor_quick` | no server at all (`case_doctor_quick` builds its own `_ping` socket and picks its ports, so it needs `python3` and nothing else) |
| `case_container_mounts` | no server *and* no daemon — one printed `SKIP` line when there is neither a daemon nor an image |
| `case_exit_classification` | `python3` (it reads a row's `expected_stop` out of `app.db`), skips without it |
| `case_egress_proxy` | **no** server, daemon, internet or key: it starts the artifact as `proxy egress` on a loopback port and asks it four questions over a socket |
| `case_schema_newer` | no daemon; boots the artifact over a schema-99 database |
| `case_key_proxy_request` | a stand-in `openssl` on `PATH`; asserts the bytes the key proxy really sends |
| `case_server_deploy`, `case_server_service` | two halves each: an **offline** one (`python3` for a `_ping` socket / a `/api/health` responder) and an **online** one (the daemon and an image) that drives a `--root` deploy with `deploy_machine_shims`' `tailscale` on `PATH`; they remove `hs_egress`/`hs_key` before and after, and `case_server_images` must have run first |
| `case_preview` | **its own boot** (stub backend, own data root, `preview_listen = "127.0.0.1:0"`) and, for its second half, the daemon and `agent_image` — it makes its own `s_<id>` container on an `--internal` `agents_<id>` and prints one `SKIP` line without them; the first half always runs and puts the shared boot back |

A story is not done until the gates are green on a clean tree, and then you run the story's own demo
yourself and paste its output.

**Every story adds at least one `case_*`** — the rule is in bold in `docs/build.md` and the reviewer enforces
it. Two shapes to avoid: a case that *needs* a session created two cases earlier, and a case whose `SKIP`
would also fire when the feature it checks is deleted (§6).

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
  migrations/0004_expected_stop.sql  sessions.expected_stop (schema 4, HS-030)
  src/main.rs               dispatcher only
  src/cli.rs                clap: version, config {validate,show,edit}, serve [--port|--data-dir|--backend],
                            proxy {egress --listen --allow}
  src/commands/             version.rs, config.rs, serve.rs, proxy.rs, doctor.rs, key.rs, container.rs,
                            deploy.rs (the seven steps' command: the config, the tail, the resume command),
                            drain.rs (`holosmith drain [--wait]`: a std UnixStream client, and the exit code
                            is the server's terminal word — 0 only for `drained`)
  src/config/               mod.rs (typed Config, KeySpec walk, 32 keys), schema.rs, problem.rs,
                            render.rs, duration.rs, editor.rs
  src/backend/              mod.rs (Step, SessionBackend, `ProcessEnd`/`ExitWatcher`/`supervise`,
                            Backends::for_run(backend, ContainerSettings),
                            BackendError, TurnEnd, Provisioned), stub.rs (the scripted turn;
                            `ignoring_abort`, `failing_at`, `failing_snapshot`), docker.rs (all four steps and
                            a turn are real: `start_container` = `rm -f` then an attached `run`; the
                            per-session start lock, which the **network** step takes too since HS-037;
                            `stop` = `docker stop --time 10`; `resume` = network + proxy + container;
                            `apply_proxy` joins the egress proxy to the session network only when the row's
                            `network` flag is set, and `remove_network` leaves it before the `rm`)
  src/provisioner.rs        the four-step background task and its five session.status events; stops at
                            a step boundary when the row leaves `starting`; after the pi step it spawns the
                            task that awaits the backend's `ExitWatcher` and calls `Exits::record` (HS-031);
                            `with_settle_reason` so a startup recovery's `starting -> idle` carries
                            `interrupted` (HS-044)
  src/reconcile.rs          the startup pass (HS-044, ADR-0053): `recoveries` (the live rows), the pure
                            `containers_to_remove`/`networks_to_remove`/`orphan_worktrees`, `sweep_worktrees`
                            (git's `worktree remove` for a registered orphan, `remove_dir_all` for an
                            unregistered one, never a branch or a ref), `Reconciler::{new, run}` and
                            `INTERRUPTED`; awaited by `serve` before the first request and before either
                            policy starts
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
  src/stop.rs               the one way a container is stopped (HS-030): `Stopper { backend, database }`,
                            `stop` writes `sessions.expected_stop` and *then* calls the backend's `stop`.
                            The sweeper, the create's eviction and the ladder's forced rung all use it, so
                            spec item 6's "before every `docker stop -t 10`" has one home
  src/tool.rs               a tool call's output as a file (HS-033, ADR-0041) and its images as files beside it
                            (HS-035, ADR-0043): `Store::new/store/read/path`, `store_images`/`image`/`read_image`,
                            `preview`/`truncate`/`valid_id`/`floor_boundary`, `decode_base64`, `CAP`/`IMAGE_CAP`
                            (5 MiB), `PREVIEW_MAX`/`ARGUMENTS_MAX` (2000), `MAX_ID` (128), `truncated_marker`,
                            `Stored { preview, reference }`, `Image`/`StoredImage`, `sweep`
  src/exit.rs               exit classification (HS-030, ADR-0038): `Exit::{Expected,Clean,Failed}`, the
                            lifted `SessionLog` (HS-031: the backend appends the launch line), `Exits::for_database`,
                            pure `classify(expected_stop, code, stderr)`, `PI_EXIT`, `STDERR_LINES` (5),
                            `HEADLINE`-free `session_dir`/`LOG_FILE`, and `Exits::record(session, code,
                            stderr)` which takes the flag, applies `PiExit` or `UnexpectedExit`, and appends
                            the stderr to `<data>/sessions/<id>/session.log`. Its caller is the task the
                            provisioner spawns after the pi step (ADR-0039)
  src/drain.rs              the drain (HS-051, ADR-0060): `Control` (the process's `Arc<AtomicBool>` — never a
                            row), `Request` (`drain <millis>`, one line), `Drain` (set the flag, poll
                            `counts_by_status` for `running` every 100 ms, then `Trigger::Drain` +
                            `Stopper` per idle row), `Ending` (`drained`/`expired`/`refused`, and the exit
                            code), `LineSink`, `listen`/`prepare`/`spawn`/`answer` (the unix socket: 0600 in a
                            0700 directory, a stale socket replaced, a live one refused, a regular file named
                            and left alone), `SERVER_RESTART` ("server_restart"), `SOCKET_NAME`, `POLL`,
                            `REQUEST_TIMEOUT`, `MAX_REQUEST`
  src/deleter.rs            the delete flow's task (HS-019): revoke_key (HS-039's dummy) →
                            `backend.delete` → remove the row *and its event log* in one
                            transaction → close the session's streams. `DELETED` is the rung-2 word
  src/agent.rs              the agent process (HS-031, ADR-0039) and its record seam (HS-032): `Agent::launch`
                            (piped stdio, an LF-only JSONL reader that routes `response` records by command id
                            and **publishes** the rest, stderr into a 64 KiB tail, the exit sent after the last
                            record), `records()`/`Slots`/`Received`/`AgentRecords`/`RECORDS_BUFFER` (a bounded
                            channel the reader *waits* on), `request`, `ready`, `Ended`, `AgentError`,
                            `HOME`/`SETTINGS`/`TEMPLATE`/`ENVIRONMENT`/`COMMAND`,
                            `home`/`ensure_home`/`seed`/`launch_line`, `version_of`/`version_check`
                            /`version_sentence`
  templates/pi-home/settings.json  the seed's template, embedded with `include_str!` (no secrets)
  images/{proxy,agent}.Dockerfile  the two recipes `server deploy`'s step 3 builds from (HS-048, ADR-0057),
                            `include_str!`ed by `src/images.rs`; markers greped out of the artifact by
                            `scripts/check-static.sh`
  src/images.rs             the image builds (HS-048, ADR-0057): the two names, `Kind`, the five `LABEL_*`,
                            `Expectation` (`of`/`of_digest`/`mismatches`/`unbuilt`/`build_args`), `digest`/
                            `own_digest` (a hand-written SHA-256), `archive` (ustar with uid/gid/mtime zero),
                            `build_arguments`, `failure`, `Status`/`read`, `architecture`, `ensure`, the
                            `Images` seam
  src/container.rs          the session's container (HS-027/028/029, ADR-0035/0036/0037): `KeyProxy::mount` is
                            the **directory** `<data>/secrets` since HS-040 (a file mount pins the inode the store
                            renames away); `Mounts`
                            (spec item 1's four, host path == container path except `/work`),
                            `Mount::argument`/`Mounts::check` (refuses a missing or wrong-kind source
                            *before* `docker run`), `Limits`, `ContainerSettings` (`of(config)`, `at(root)`,
                            `docker()`), `SessionContainer::run_args` (the whole `docker run` vector:
                            `-i --rm --init --name s_<id> --network agents_<id>`, the hardening, the
                            three limits, `-w /work`, `-e HS_SESSION_ID`, pi's five variables and
                            `HTTPS_PROXY`), `network_name`, `Unmountable`; and since HS-037 the machine's
                            proxy: `EgressProxy` (`of(&Config)`, `run_args`), `PROXY_NAME` (`hs_egress`),
                            `PROXY_ALIAS` (`egress-proxy`), `PROXY_ENV`, `proxy_url(id)`,
                            `PROXY_START_TIMEOUT` (90 s); and since HS-039 the key proxy:
                            `KeyProxy` (`of(&Config)`, `run_args`), which mounts the key file read-only
                            and runs `proxy key` on `hs_key`
  src/vapid.rs              the machine's Web Push key pair (HS-049, ADR-0058): `VAPID_FILE` (`vapid.toml`),
                            `Pair`, `store`, `render`, `parse`, `is_pair`, `from_der` (the SEC1 walk), `base64url`/
                            `unbase64url`; unit tests for the DER vector, RFC 4648's vectors and the three refusals
  src/key.rs                the provider/store half of HS-039 (ADR-0048), whose table gains `opencode-go` for
                            HS-040 (host `opencode.ai`, `/inference/go/anthropic`, `x-api-key`; ADR-0050):
                            `Provider`, `PROVIDERS`,
                            `provider`, `Store` (`from_data_root`, `ensure_file`, `load`, `save`,
                            `set_provider`, `remove_provider`, `providers`, `dummy_for`, `revoke`,
                            `session_of`), `Secrets`, `models`, `KEY_PROXY_ALIAS`, `KEY_PROXY_PORT`,
                            `KEY_PROXY_NAME`, `contains_real_key`
  src/keyproxy.rs           the key proxy itself (HS-039, ADR-0048): `Route::parse`, `Head`/`parse`,
                            `upstream` (header swap **and the body**: it returns head+body as one value since
                            HS-040, whose V3 run measured a `Content-Length` with no bytes behind it),
                            `serve`/`handle`, `forward` (openssl s_client, counted streaming), `Record`,
                            `MAX_BODY`, `MAX_CONNECTIONS`, `CA_FILE`
  src/snapshot.rs           the WIP snapshot (HS-042, ADR-0049): `take` (temporary index, read-tree,
                            add -A, write-tree, commit-tree, update-ref), `Taken::{Written, Removed}`,
                            `SnapshotError`, `REF_PREFIX`, `MESSAGE`, `IDENTITY`, `reference`
  src/commands/key.rs       `holosmith key set|list|remove` (HS-039): stdin-only set, providers only in
                            list, remove
  src/commands/container.rs `holosmith container snapshot` (HS-042): `/work` + `HS_SESSION_ID` + a
                            temporary index
  src/docker.rs             the `docker` CLI as a program (HS-028): `Docker{host}`, `run`/`succeeds`,
                            `ensure_network`/`gateway` and `EGRESS_NETWORK` (HS-036), `command_line` (the one
                            call whose stdio a caller keeps: the launch),
                            `create_internal_network`/`remove_network`, `remove_container`/`stop_container`
                            (`STOP_SECONDS` = 10), `inspect`, `DockerError`; and since HS-037 the bounded
                            family for the startup path — `bounded` (the primitive), `attempt` (the probe),
                            `run_detached(args, bound)`, `removed_bounded`, `remove_container_bounded`,
                            `remove_network_bounded`, `running`, `join_network`/`leave_network` (alias,
                            disconnect-first, force), and since HS-044 the two `--format` listings
                            `container_names`/`network_names`
  src/compactor.rs          compaction's task (HS-060, ADR-0032; `foldable` learns `starting` in HS-044,
                            because a recovery parks every live row there): `SNAPSHOT_AFTER` (24 h, a constant),
                            `TICK` (60 s), `MESSAGES_PER_PASS` (500), `Compactor::{new, with_window,
                            spawn, run, sweep, fold}`. A pass at startup, then a tick; the per-session
                            step holds the database mutex, so a running turn is skipped by the re-read
  src/backup.rs             the project backup (HS-065, ADR-0034): `Backups::{new, take, prune}`,
                            `BackupError`, `KEEP` (7), `BACKUPS` ("backups"), and the private `day`/
                            `civil_from_days` (UTC, integer-only, no dependency). Sync and std-only, and
                            it logs nothing — the create logs its error
  src/http/tool_output.rs   one tool's stored output (HS-033): 404 for an unknown session, an id that cannot be
                            a filename and a missing file; `text/plain; charset=utf-8` otherwise
  src/rpc.rs                the mapping (HS-032, ADR-0040; HS-033's tool pair in ADR-0041; HS-034's usage
                            arithmetic in ADR-0042; HS-035's images and dialogs in ADR-0043):
                            `Mapping::{new, map, skipped, unknown}` (one pi record → at most one frame,
                            `Mapped::{Nothing, Frame, Settled, Tool, Cancel}`), `pump(records, mapping, events,
                            store, agent)` (turn.start → frames → turn.end, ending on `agent_settled`), the
                            minted message id, `declined`/`unknown` counters, `ToolEnd`/`result_images`,
                            `Cancel`/`cancelled_reason`, the two additive keys on `message.end` and the two on
                            `tool.end`
  src/http/tool_image.rs    one image a tool returned (HS-035): 404 for an unknown session, an id that cannot
                            be a filename, a non-numeric index and a missing file; the file's own type otherwise
  src/runtime.rs            runtime::Backend {Stub,Docker} — which backend this run uses
  src/http/                 mod.rs, app.rs (Routes/App seam, AppState + provisioner()/turner()/
                            transitioner()/ladder()/deleter()/sweeper()/compactor(), edge layer,
                            is_guard_exempt), error.rs
                            (the one envelope, every code, ApiError::not_in_state / bad_prompt /
                            bad_steer / unsaved_work / unknown_session / cap_without_idle /
                            git_not_directory / submodules_present / worktree_config_enabled),
                            health.rs, guard.rs (the four checks), sessions.rs (POST+GET
                            /api/sessions, GET /api/sessions/{id}, POST …/prompt, …/abort, …/steer,
                            …/resume, DELETE /api/sessions/{id}[?force=true], TURN_STEER, EVICTED,
                            `Slot` + `slot_for` + `evict_one`, `apply_trigger`, and the project's
                            refusals: `refuse_project` + `refusal_error`), streams.rs (the SSE
                            handler)
  src/sessions/             mod.rs (Status, Session, NewSession, RepoError, `is_id`), repository.rs (every
                            `sessions` statement + its SQL-location guard; includes touch,
                            `oldest_idle`, `last_seq` (HS-044's reader), and `remove` — the row and its log in
                            one transaction),
                            state.rs (the transition table, Transitioner::apply / apply_logged, the
                            set_status guard), activity.rs (the one `touch`: lock, warn, carry on)
  src/events/               mod.rs (Event, NewEvent, EventWriter, EventStream, and since HS-059
                            `coalesce` + its private `parse_delta`/`Delta`, which HS-060's fold reads
                            too), writer.rs (`collect` →
                            `coalesce` → `commit`; the failure log names appended events *and* rows),
                            repository.rs (`insert_batch` drops an event whose session has gone;
                            `delete_session_log`; `compact`, the fold into one snapshot; since HS-061
                            `truncation_point`/`turns_before`/`read_before`; and `usage`/`message_id`
                            — the three payload shapes it parses, ADR-0032), stream.rs (replay→live,
                            lag, `Item::Truncated`/`HISTORY_TRUNCATED`, the cut in the read step),
                            registry.rs (open-stream counts)
  src/worktree.rs           the session's worktree (HS-025, ADR-0029), and since HS-044 `registrations`
                            reads git's own `worktree list --porcelain` for the startup sweep:
                            `Worktree::of` (the naming rule; it
                            carries `id` since HS-043), `ensure` (create or adopt, `Ensured { base_commit,
                            created }`), `ahead` (`rev-list --count`), `remove(&Retained)` (registered? →
                            remove → branch −D **only when not kept** → prune), and `GitError`; since
                            HS-026 the refusal reads (ADR-0030): `refuse`, `Refusal`, `repository_of`,
                            `CONFIG_READ` (`--local --type=bool`); and since HS-043 the keep-rule
                            (ADR-0052): `retained(base, targets)` → `Retained::{Known { branch, wip },
                            Unknown}` (`merges_into` per existing target, `differing_ref` by tree) and
                            `Retained::{keeps_branch, recovery}`. Sync and std-only; the backend calls it on
                            `spawn_blocking`
  src/egress.rs             the egress proxy (HS-037, ADR-0045): `Request::parse`/`username`, `Rules`
                            (`new`/`allow`/`permits`/`target`), `Target`, `Refusal` (status, word, sentence),
                            `address_problem`/`embedded_v4` (the ranges, IPv4-mapped, 6to4/NAT64/compatible),
                            `Record` (`unreadable`/`tunnel`/`log`), `serve`/`handle`/`read_head`/`resolve`/
                            `connect`/`establish`/`respond`, `pump`/`pump_both` (counted both ways),
                            `PORT`/`TLS_PORT`/`DEFAULT_LISTEN`/`MAX_HEAD`/`HEAD_TIMEOUT`/`ACCEPT_BACKOFF`
  src/selftest.rs           the network self-test (HS-036, ADR-0044; HS-038's pair, ADR-0046; HS-039's
                            inverse pair, ADR-0048): `Runner` (the seam, now with `proxy`/`join_proxy`/
                            `leave_proxy`/`key_proxy`/`join_key_proxy`/`leave_key_proxy`), `Report`,
                            `Outcome::{Passed, Failed, Unknown}`, `run`/`run_with`/`decide`/`leaked`, `pair`,
                            `key_pair`, `NETWORK`, `PUBLIC`, `PROXY_TARGET` (`egress-proxy:3128`),
                            `KEY_PROXY_TARGET` (`key-proxy:8080`), `HOST_PORT`,
                            `REACHED` (9), `PROBE_TIMEOUT`, `PROBE` (`include_str!` of
                            `templates/selftest-probe.js`)
  src/preflight.rs          the three startup checks and their sentences (HS-024, ADR-0028), and since HS-045
                            the vocabulary `doctor` prints too: `Finding { check, outcome, sentence }`,
                            `Outcome { Pass, Fail, Unknown }`, `report_with` (each check has one function with
                            both arms) and `check`/`check_with` = its failures via `Complaint::of` (the failing
                            projection, same `String`); `Tools { git, df }` is the tests' shim seam, `Check`
                            is the **one** word table (docker, git, disk, network, systemd, merge, tailscale,
                            ports, database, proxy, image), `socket_sentence`, `MIN_GIT` (2.38)
  src/doctor.rs             `holosmith doctor`'s list (HS-045, ADR-0054): `Depth {Quick, Full}`,
                            `Tools { preflight, tailscale, systemd, config_path, admin_dir }` (the last two are the
                            install tree's machine-wide paths, so `perms` reads the *caller's* tree — HS-049),
                            `perms` (the installed paths' modes and owners, the data root's owner as the reference),
                            the `Machine` seam (`running`,
                            `image_pi`, `selftest`, implemented by `Docker`), `run(config, depth, tools,
                            machine) -> Report`, `Report::{failed, failures, render}`; the checks it owns —
                            systemd (`/run/systemd/system`), the merge trial (a throwaway repo:
                            clean 0, conflict 1 with the path), Tailscale (`status --json`'s `BackendState`),
                            the two ports (free before a start, held by this machine's own `/api/health`
                            after one), the database (`db::recorded_without_migrating` + `schema::verdict`),
                            the self-test, `hs_egress`/`hs_key`, the image's pi; `SYSTEMD_DIR`, `PROBE_TIMEOUT`
  src/commands/doctor.rs    the subcommand: config, `doctor::run`, print, exit 1 iff a finding failed
  src/db.rs                 pragmas + read-back, apply_pending (which switches on `schema::verdict`),
                            `recorded_without_migrating` (SQLITE_OPEN_READ_ONLY: `doctor`'s reader, which must
                            not migrate), Database::sessions()/events()
  src/sql.rs                the one SQL statement scanner + the shared source walk (cfg(test) guard)
  src/schema.rs             SCHEMA_VERSION (= 4), include_dir! embed, check_embed, contract_violations,
                            and since HS-075 `Verdict {Same, Older, Newer}`, `verdict` and `Verdict::sentence`
                            (the sentence the boot warns with and `doctor` refuses with, ADR-0047)
  docs/schema/previous.sql   the previous release's schema as a dump (HS-075, ADR-0047): compared with this
                            build's by `tests/schema_previous.rs` as a subset; NOT a migration, and it must
                            never move under `migrations/`
  scripts/schema-snapshot.sh  the release step that regenerates it (`<data root>/app.db`, read-only open)
  src/{serve,web,logging,clock,version,pins}.rs
  tests/                    version_cli, config_cli, http_contract, serve_cli, db_migrations, exit, agent,
                            rpc (the fixture replays and the pump), tool_output (the endpoint and the store's
                            rules at the wire), key_cli, key_proxy, snapshot, fixtures/pi-turn-{text,thinking,
                            tool,image,extension-ui}.jsonl, pin_single_source, guards, session_repository,
                            sessions_api, drain (the socket, the wait, the stop and the refusals), drain_cli
                            (the printed lines and the exit code against a hand-written socket), events,
                            streams, provisioning, state_machine, session_list, session_screen,
                            turns, abort, steer, resume, delete, eviction, idle_stop, turn_timeout,
                            deploy (the seven steps over shims), images (the embedded consts, the ustar, the
                            failure sentence, and a daemon-gated build of a test tag), doctor,
                            token_budget, preflight, doctor (every check's arms over shims, a fake `Machine`,
                            a real bound port and a real health listener, a real database in each schema state,
                            and a real git for the merge trial), backend_contract, worktree, reconcile (the row half and
                            the worktree sweep, no daemon), compaction,
                            history, backup, schema_previous (the subset test + the comparison function +
                            the verdict's sentences), container (V1 + the container's flags + stop/remove/resume +
                            eight concurrent starts; **needs a daemon and `$HOLOSMITH_TEST_IMAGE`,
                            default `pi-agent:local`, and skips otherwise**), support/mod.rs
                            (the shared harness, which also has `repository`/`git`/`head`/
                            `branch_exists`/`worktrees` helpers for a real git repo in a `TempDir`)
web/                        Vite 7 + TypeScript, two screens (`/` and `/sessions/<id>`)
                            package-lock COMMITTED; main.ts is the DOM, transcript.ts the renderer's
                            rules (frame parsing, block model, affordances, `stoppedNotice` /
                            `stoppedDetail`, `budgetNotice`, `createFailure`, `parseTruncated`,
                            `setTurnBase`, `historyPath`, since HS-030 `stateText`/`headline` — a
                            multi-line reason renders as its last line, clipped at `REASON_MAX` — and since
                            HS-043 `deletedNotice`/`Kept`, `UNSAVED_WORK_NOTICE`, `FORCED_DELETE_LABEL`;
                            `main.ts`'s `deleteSection` is the delete affordance, and `StreamTarget` carries
                            one `showState` that patches badge, preview, composer and the affordance)
web/test/transcript.test.ts 52 tests over synthetic frames, node's own runner (`npm test`)
scripts/build-web.sh        npm ci + npm test + npm run build, asserts web/dist/index.html
scripts/build.sh            web first, HOLOSMITH_REQUIRE_WEB=1, both musl targets → dist/<triple>/,
                            atomic copy, then check-static.sh. Uses `cargo build --locked`.
scripts/check-static.sh     file/readelf, empty-root serve + cmp against web/dist/index.html, marker
                            gate, app.db assertions, byte grep for the schema SQL, Cargo.lock guard
scripts/check-embed.sh      the three embed cases (present / absent warns / required fails)
scripts/verify.sh           the one-command gate (see §3), including the two stand-ins HS-049 added:
                            `deploy_machine_shims` (a `tailscale` that records argv and answers `serve status
                            --json` from the pairs it was asked for) and `health_responder` (a `python3` that
                            answers `/api/health` for a whole case)
scripts/import-linear.py    the MCP plumbing /tmp/lin.py reuses
docs/adr/0001…0060          read the recent ones before planning anything
docs/build.md               prerequisites, gates, verify.sh, guards, migrations, placeholders
docs/review/PER-33…PER-83, PER-91…PER-93, PER-97, PER-107  each a verbatim review + a `## Resolution` table
docs/stories/               the 80 story slices — the dependency order and definition of done
GLOSSARY.md                 the domain vocabulary. Check it BEFORE inventing a term.
```

### What exists and what does not

- **Working:** `version` (text + `--json`), `config {validate,show,edit}`, `serve` (control listener,
  embedded app, `/api/health`, `POST`/`GET /api/sessions`, `GET /api/sessions/{id}`,
  `POST /api/sessions/{id}/{prompt,abort,steer,resume}`, `DELETE /api/sessions/{id}[?force=true]`,
  `GET /api/sessions/{id}/events`, `…/events/history`, `…/tool-output/{tool_id}`,
  `…/tool-image/{tool_id}/{index}`), SQLite with **four** migrations, the guards, the event writer (which
  coalesces a commit's deltas), the SSE stream, the stub backend and its four-step provisioner, the
  enforced state machine, the abort ladder, the delete flow, the cap with eviction, the create's and the
  resume's three project refusals, the compaction task (which folds a day-old message into one
  `message.snapshot`), the Sessions screen with its create form (which prints a project
  refusal's sentence alone) and the "nothing was deleted" notice, the session screen at
  `/sessions/<id>` with a live transcript and a four-action composer.
- **The agent is real (HS-031) and so is a turn (HS-032 – HS-034).** A Docker create provisions the worktree,
  the network, the attached container running pi in RPC mode, and pi's ready handshake, and ends **`idle`**;
  the agent's directory and the launch line are under the data root, the exit the attached stdio observes is
  classified, and a resume replaces the container and reaches `idle` again. A **prompt** reaches pi, its
  records become the transcript's frames, a tool call's output is filed, and the session's spend is counted —
  with one gap a real container still shows: pi refuses a `prompt` before a stream exists, because a session's
  network is `--internal`, its pi-home has only a dummy key, and the gate's machine stores no provider key, so
  `case_docker_turn` asserts pi's own refusal sentence and the streaming evidence is the committed fixtures —
  while HS-040's recorded run is the evidence that a real turn works end to end through the key proxy (text and
  thinking deltas, a tool round trip, usage), because the gate may not hold a provider key.
- **The egress proxy exists and is the only route out (HS-037).** `holosmith proxy egress` is a mode of the
  artifact; a Docker boot starts `hs_egress` on `egress` (bounded, `warn` on failure) and the network step
  joins it to a session's network under the alias `egress-proxy` **only when the row's `network` flag is set**,
  so `HTTPS_PROXY` is the same string either way and the difference is topology. HS-038 measured that (the boot's
  bare/joined pair and `case_internet_toggle`) and left the **copy** under the checkbox to whoever next owns the
  create form, because a new element is a UI change in a phase that forbids one (ADR-0046).
- **The migration rule has two halves and both are enforced (HS-075).** `schema::contract_violations` refuses an
  unmarked destructive statement in a migration (`cargo test`), and `tests/schema_previous.rs` refuses a drop or a
  rename against `docs/schema/previous.sql` — the gate calls the second the `migrations` **phase**. Refreshing
  that snapshot is a **release step** (`scripts/schema-snapshot.sh`), not a story's, and a contract migration
  changes it in the same commit. `schema::verdict` is the sentence `serve` warns with on a newer database and
  `doctor` refuses with (ADR-0047) — kept by HS-045 without migrating the file.
- **The startup reconcile landed (HS-044).** `serve` awaits `Reconciler::run` right after the state exists and
  before the sweeper and the compactor are spawned: every live row goes to `starting` (with `interrupted` when a
  turn was in flight), the names no row owns are swept (a container unless its row is a `deleting` delete in
  progress; a network and a worktree directory only when no row has the id; never a branch or a ref), and the
  sessions are provisioned again. **What is left of the network half:** nothing but the phase-1 verification
  run (HS-046), which is this handoff's §1 — `doctor` was HS-045. The delete's keep-rules (HS-043) are done and ADR-0052: `agent/<id>` survives a
  delete when it is ahead of its base and in no target, `refs/holosmith/wip/<id>` survives when its tree
  differs from the branch, the 202 carries `kept` (one additive key on the flattened row) and is the same
  decision the spawned removal applies, and the session screen has the **delete affordance it did not have**
  — a Delete button, a confirm step and the forced retry a `409 unsaved_work` earns. **V9 passes** (HS-044);
  nothing in the spec's table is unrun now except V1, V2, V6, V8 and V10's *Result cells* (HS-046 fills them;
  HS-045 built V2's check) and V11 (phase 3). `doctor --full` (HS-045) is what makes V2 — and V10's build, and
  the self-test — repeatable rather than a ceremony. The preview listener (HS-041) is
  done: the binary's third byte tunnel, `/‹host›/‹id›/*` with the prefix stripped and an upgrade's `Origin`
  checked, and **V7 passes** (the host reaches a container on an
  `--internal` network, measured directly and through the artifact). The key proxy (HS-039) and its
  **verification** (HS-040 — V3, V4 and V5 pass) and the WIP snapshot (HS-042) are done, and the spec's
  verification table has a **Result** column that HS-046 finishes filling.
- **Phase 1 is closed by the verification run (HS-046).** `HolosmithSpec.md`'s **Vertical slice verification**
  table is the record: every `V1`–`V10` row carries a verdict and the automation that produced it (a `case_*`, a
  `tests/*.rs`, a `scripts/*.sh`, or `docs/build.md`'s by-hand section), and `V11` still says `deferred to phase 3
  (HS-070)`. V3, V4 and V5 were **not** re-measured by that story — no provider key exists here and no gate may
  hold one — so they quote HS-040's by-hand run and say so. `case_verification_table` is what keeps the table
  honest (ADR-0055), and the gate now prints one `ok <case>` line per case that measured something, withholding
  it for a case that printed `SKIP`.
- **`doctor` exists (HS-045).** `holosmith doctor [--quick|--full]` prints one line per check —
  `check: pass|fail|unknown: sentence` — after a `doctor: <depth>` header and a tally, and exits 1 iff a finding
  failed. `--quick` is the list `serve` refuses on (same `Finding`/`Complaint`, so the sentence is byte-identical)
  plus systemd, the merge trial, Tailscale, the two listeners and the database's schema; `--full` adds the
  network self-test (whose bare probe *is* the network-off probe), `hs_egress`/`hs_key` and the image's pi. On
  this machine `systemd` and `tailscale` are `unknown` (no systemd; no `tailscaled` answering), which is the
  designed outcome, not a failure. Ports are two moments: free before a start, held by this machine's own
  `/api/health` after one. It starts nothing that outlives it. `ExecStartPre=holosmith doctor --quick` is
  **HS-049's** line to write.
- **The log is now read in two shapes and written by two producers of two kinds.** The stream replays a
  *window* (`history_window`, default 50) and says so with `history.truncated`; `GET …/events/history`
  answers the frames below that cursor. `events/stream.rs`'s `REPLAY_LIMIT` is still `u32::MAX` — it is
  "everything from the cut", and a second window there would be an invisible truncation. The project
  backup is a second *writer* of files under the data root (`<data>/backups/`), not of the database.
- **A stop is now recorded as the server's own, and a clean exit has its own state row — observed.**
  `Stopper` writes `expected_stop` before the `docker stop`, and `Exits::record` turns an exit into `stopped`
  (expected), `stopped · pi_exit` (code 0, no flag) or `error` + the last stderr lines. Since HS-031 its
  caller is the task the provisioner spawns after a successful pi step, fed by the backend's `ExitWatcher`
  (ADR-0039). Do not "fix" anything by adding a `docker events` watcher: the attached process *is* the
  observation.
- **Five policies can now stop something and they must not be confused:** the cap evicts (`evicted`,
  inside a create), the sweeper stops a quiet one (`idle_stop`, on its own tick), the turn timeout ends a
  *turn* (`turn_timeout`, from the runner), the token budget ends a *turn* (`budget_exceeded`, also from
  the runner, on one `fired` shared with the timeout so only one climb happens) and the drain stops every
  `idle` row an operator asked for (`server_restart`, over the admin socket). All of them go through
  `Transitioner::apply`, and the row moves before the container in every one of them.
- **`runtime::Backend`** has `Stub` and `Docker`; `serve --backend` and the config key `backend` select
  it. `DockerBackend::new(ContainerSettings)` provisions all four steps, runs a turn's five operations and
  implements the WIP snapshot. The default backend is **`docker`** (ADR-0001: the stub is opt-in), so a
  default build provisions a session to `error` with a reason rather than hanging in `starting`, and
  `scripts/verify.sh` boots with `backend = "stub"` in its config (so does `scripts/check-static.sh`'s
  empty-root run, because a Docker run refuses at startup on a machine that fails the preflight, HS-024, and
  that sandbox has no daemon, no `git` and no `df`). (Since HS-026 a create for a project that is not a plain
  checkout is refused earlier, with a 422; the demo trick HS-019 used still works for a project that *is*
  one — which is also how HS-019 provoked a real failed snapshot from the shipped artifact.)
- **`AppState`** is `{ config, runtime, database: Arc<std::sync::Mutex<Database>>, events: EventWriter,
  streams: StreamRegistry, backend: Backends }`, with **eight** wiring helpers — `provisioner()`,
  `turner()`, `transitioner()`, `ladder()`, `deleter()`, `sweeper()` (an `Option`, `None` when
  `idle_stop_min` is 0), `compactor()` (always `Some`) and `reconciler()` (HS-044). The mutex is an explicit
  stopgap (ADR-0012/0013; `docs/notes.md`'s *Data and state* landmines).
- **`sessions::state::Transitioner::apply`** is the only way a session's state changes; `apply_logged`
  is the runners' wrapper (log, never return). A provisioning step report is `from == to` with `step`
  set and is *not* a table row.
- **The create is one lock scope plus a loop.** `create_locked` answers `Slot::{Inserted, Existing,
  Full}`; a `Full` makes `slot_for` evict one idle session (through `apply`, which takes the lock
  itself) and try again. Idempotency is checked first, so a retry never evicts anyone. `evict_one`
  moves the row **before** it spawns the backend's `stop`, because the row is what frees the slot.
- **`config`** has (`32` keys now) `agent_image` (default `agent-image:<binary version>`; this machine's
  demos and the gate set `pi-agent:local`), `container_cpus` (2), `container_memory_mb` (4096),
  `container_pids` (512) — all three **refuse `0`** — plus `abort_grace` (default `10s`, **used** by the ladder), `idle_stop_min`,
  `stopped_ttl_days`, `min_free_gb`, `turn_timeout_min`, `max_session_tokens` and the rest — check
  `config show` before adding a key. The delete flow and **compaction** have **no** config key of their
  own (the 24-hour window is a constant), and `max_live_sessions` (default 3, ≥ 1) is the cap.
- **`POST /resume`** takes the transition, then spawns the provisioner, and **touches `last_active_at`**
  (a resumed session must not be the eviction's first candidate — which is now a real policy, not a
  future one). The cap is checked **before** the transition, and the resume answers `409 cap_reached`
  rather than evicting (ADR-0022, ADR-0024). `SessionBackend::resume` has had a body since HS-029 and is how
  a `stopped` row comes back.

---

## 5. The cycle — one issue, then write the state back

Everything is **sequential: one writer at a time**, never two children editing files. Review children are
read-only and may run in parallel.

1. **Confirm the issue.** Read §1's slice, the Linear body (`python3 /tmp/lin.py issue <IDENT>`, §2) and the
   spec items. **Check the issue's title against the story you mean** — the Linear number is not derivable
   from the `HS-` number (HS-059 is `PER-91`, not `PER-93`; a session wrote `Closes PER-93` for it and had to
   record the correction in three places). Note `HEAD` — that SHA is the review's fixed point.
2. **Plan.** For an **L** story, give a fresh `delegate` child a grilling brief
   (`~/.agents/skills/grilling/SKILL.md` + `domain-modeling`): it puts **one** round of questions to *you*
   via `contact_supervisor`, and you answer with `subagent_supervisor({action:"reply", …})`. For **S/M**
   (HS-043 is M), write the plan yourself — it is faster and the analysis is usually already done. Either
   way: state the environment facts, name the files to read, list the decisions already settled, and
   **pre-answer the obvious round-2 questions**. A planner may write only `GLOSSARY.md` and a new
   `docs/adr/*.md`.
3. **Record in Linear before any code exists**: `save_comment` with the plan shape, a decisions→precedent
   table, the acceptance restated as testable checks, the non-goals, the docs touched, the demo, and the
   follow-ups; then `state` the issue `In Progress` and re-read it to confirm.
4. **Implement.** Two proven patterns: **S/M in-session** (most stories since HS-016), or **M/L with a lot
   of new surface** handed to a fresh `worker` child with a *written* brief — the plan plus the "how", the
   ground rules (no commit, no Linear, no new dependency, `env.sh` first, don't edit `docs/stories/**`), the
   exact gate commands, and "budget discipline: keep the tree compiling; if you run long, stop green and
   report which steps remain". Twice a child found a genuine contradiction in the brief — verify its claims
   rather than trusting them.
5. **Verify yourself.** Never trust a child's pasted output: run the gates and the demo. This has caught
   something every time (a wrong count, a demo-revealed message bug, a panic path, an assertion that passed
   over an already-stopped row).
6. **Review** with a fresh `reviewer` child running `~/.agents/skills/code-review/SKILL.md`, read-only,
   against `<fixed point>...HEAD` plus untracked files, spec axis sourced from the Linear body + story slice
   + spec items. `git add -A` first and write `git diff --cached <fp>` to
   `/tmp/holosmith/reviews/PER-<n>-diff.txt` so new files appear. **The reviewer has no shell and no write
   tool**: check its claims, and *you* save the review verbatim to `docs/review/PER-<n>.md`. **Commit before
   launching it** — a child reading a tree that moves under it stalls.
7. **Fix** in the same session (or with a fresh `worker`), triaging every finding as fixed or
   dismissed-with-citation; a "won't fix" without a citation is not acceptable. Re-run the gates. Append a
   `## Resolution` table to the review file and commit it with the fixes. If a claim cannot be dismissed,
   mutate the code and watch the guarding test go red before believing it.
8. **Ship**: focused conventional commits that name the issue, the last one ending `Closes <IDENT>` (§1's
   identifier). Commit messages are prose about *why*, not a restatement of the diff — look at the last few
   for tone. Rebuild after committing (§3). Then `save_comment` with what shipped, the gates, the verbatim
   demo output, the acceptance item by item and the follow-ups; set the issue `Done`; re-read it to confirm.
9. **Push**, with the sandbox's single-key loop (§2). If the push fails, report it — never fake one.
10. **Write the state back.** Rewrite §1 of this file for the next issue (§1.10's shape), refresh
    the counts, the ADR number, the last gate run's numbers and §9's row, then commit that with the message
    `Refresh the handoff after <IDENT>`. Set §1's replacement by `PROMPT.md`'s ranking (Linear priority →
    `HS-###` order → anything already `In Progress` → skip an issue whose `Blocked by` stories are not `Done`).
11. **Kill every child you started, then stop.** This is the last thing a session does, and it is not
    optional: a child still running holds the working tree, a port, a data root or a Docker network, and the
    next agent then finds a tree that moves under it, a port that is taken, or a `case_*` that fails for a
    reason that belongs to *your* session. Nothing may outlive the session that started it.
    - **Subagent children:** `subagent({action:"list"})` and `subagent({action:"stop", id})` on every run
      that is still live — the planner, every `worker`, every `reviewer`, and one you left `interrupted`. A
      run that already answered needs nothing; a run that is *listed as running* is the one that matters.
      `stop` is not resumable, which is exactly right at the end of a session: a half-finished brief is
      recoverable from Linear and `docs/review/`, a child that keeps editing the tree is not.
    - **Processes:** every demo's `serve`, every backgrounded `timeout` wrapper, `scripts/verify.sh` if you
      interrupted it, and any `node`/`python3` client you left running. **Score the pids when you start them
      and kill those pids** — `pgrep -f 'holosmith serve'` matches unrelated processes (a landmine, in
      `docs/notes.md`), so a pattern-based kill is both too broad and unreliable.
    - **Docker:** `docker rm -f` every container a demo or a test made (`s_*`, `hs_*` names of yours) and
      `docker network rm` the `agents_*` network it made; leave the machine's own `hs_egress`/`hs_key`
      alone if `serve` is not yours to stop, and never remove a container you did not create.
    - **Check it, do not assume it:** `subagent({action:"list"})` shows nothing running, the pids you scored
      are gone, `docker ps` shows none of your containers, and `git status --short` is clean apart from the
      operator's own `log.txt`/`orchestrate.sh`. Then say in one line what you killed, and stop.

Working artifacts (`/tmp` is ephemeral; the durable record is Linear + `docs/review/` + `git log`):

```text
/tmp/holosmith/plans/PER-<n>.md            the plan
/tmp/holosmith/plans/PER-<n>-linear.md     the Linear comment body
/tmp/holosmith/plans/PER-<n>-impl-brief.md the implementer's brief
/tmp/holosmith/plans/PER-<n>-ship.md       the closing Linear comment body
/tmp/holosmith/reviews/PER-<n>-diff.txt    the diff artifact for the reviewer
/tmp/holosmith/reviews/PER-<n>-review-brief.md
/tmp/holosmith/reviews/PER-<n>-review-verbatim.md
/tmp/holosmith/demo-per-<n>.sh|.py         the demo, with its client half
/tmp/holosmith/web-check*.mjs              the Chromium half, when the story has a screen
```

---

## 6. Which notes bear on this issue

The catalogue lives in **`docs/notes.md`** — read the **cross-cutting** sections before you start anything,
then the sections below. It is a reference: everything there is a measurement or a review finding.

| Read | Why it bears on HS-052's changes view |
| --- | --- |
| Cross-cutting: *Build and toolchain*, *Tests*, *Config and docs*, *Demos*, *Reviews* | True of every story. This one's sharpest: **Tests** — every acceptance bullet is a *git* fact, so the case has to own a real repository and move refs between two assertions, which is `case_worktree`'s shape (a `backend = "docker"` boot against a fake `_ping` socket runs the **worktree** step and then fails at the container step — a real worktree on a real repo with no daemon); **HTTP and the envelope** — a new refusal code, and a `409` whose sentence must name the fix. |
| *Landmines: worktrees (HS-025)* | The naming rule (`agent/<id>`, `.worktrees/<id>`), `Worktree::ensure` and its adopt-vs-create decision, `ahead` (`rev-list --count`), and the rule that a worktree is the *host's* — which is the sentence "all git work runs on the host in the project repository; no container is involved" made of. |
| *Landmines: project refusals and coalescing (HS-026, HS-059)* | `worktree::refuse`/`Refusal`/`CONFIG_READ` is how this build asks git a question whose answer a repository's own config could subvert — the same fear that makes `--no-ext-diff --no-textconv` (HS-053) and `-c core.hooksPath=/dev/null` (HS-055) necessary. `repository_of` is the host repo's path. |
| *Landmines: what a delete keeps (HS-043)* | The build already answers "is this branch merged?" — `Retained::merges_into` and `differing_ref` compare trees and refs rather than trusting a name. Read it before inventing a second definition of mergeability for `mergeable`, and note that `merge-tree --write-tree` is the *spec's* answer, so the ADR should say why the two are different questions (a delete asks "can I lose this?", the view asks "would a merge conflict?"). |
| *Landmines: the doctor, the ports and a read-only open (HS-045)* + *Landmines: `status`, `logs` and `uninstall` (HS-050)* | The three-armed fact (`pass`/`fail`/`unknown` with a sentence) and the **P1 lesson** behind it: `mergeable` is a `bool` in the spec's shape over a fallible question, and folding "could not look" into `false` is the exact mistake `Docker::running` made. Also the read-only opener and "a stat's failure is not 'not there'". |
| *Landmines: tool output, usage and the gate's shell (HS-033, HS-034)* | What a `case_*` may assume about this machine's shell: no `awk`, `python3` when a fact is easier to read than to parse, `sqlite3` for a row fact the API does not carry, and a fixture that is the case's own. |
| *Landmines: the drain and its socket (HS-051)* | The newest section, and the previous story: it is the precedent for a module whose every line is a *refusal* or a *measurement*, and its `Wait` enum is the shape "three answers, not two" takes in code. Also the landmine that matters here: a boot in a test or a gate passes `--admin-dir`, and the same discipline applies to `projects_root` in any case that owns a repo. |

Three rules worth repeating here because they are the most expensive to relearn: **a test that cannot fail
is worse than no test** (mutate the behaviour and watch it go red); **a gate case that SKIPs on the bug it
exists to catch is this project's P1**; and **after any scripted Markdown edit, read the changed lines** —
three such edits have cost a review finding each.

---

## 7. Decisions taken on the operator's behalf (do not silently undo)

Almost every hard-to-reverse decision is an ADR (`docs/adr/0001` … `0060`, one line each in
`docs/stories/README.md`): read the recent ones before planning, and **amend rather than rewrite** — an ADR
a later story changes gets a "Superseded by ADR-00xx" note (0012/0013, and 0026 amended 0020).

The handful that are **not** in an ADR, and that a fresh session is most likely to undo by accident:

- the default branch stays **`master`** — this repo has no `main`, and renaming it would be an invented
  change;
- **never fake a push**: report the failure instead (§2's loop is how a push actually works);
- **`docs/stories/**` is not edited** — it is the source the Linear issues were imported from; a contract
  that needs changing goes in an ADR;
- the default backend is **`docker`** with the stub opt-in (ADR-0001), so every test and script that boots
  `serve` says which backend it means;
- **S/M stories are implemented in-session; M/L ones go to a worker child with a written brief** (§5);
- the **Linear issue number is not derivable from the story number** — read the title before writing
  `Closes <IDENT>`;
- **one issue per session, then write the state back and stop** (this file's own rule, §5 steps 10–11);
- **nothing a session starts outlives it** — every subagent run, every demo server and every container it
  made is stopped before it stops (§5, step 11). A leftover child is not harmless: it keeps editing a tree
  the next agent has to trust.

---

## 8. Resume checklist

```bash
cd /home/adamolsson/projects/holosmith
git status --short                                  # clean apart from log.txt/orchestrate.sh
. ~/.pi/toolchains/env.sh                           # ALWAYS before cargo/rustc
python3 /tmp/lin.py issues | grep -v ' Done ' | head # recreate /tmp/lin.py first (§2)
python3 /tmp/lin.py issue PER-84                    # scoped, Backlog, the issue §1 names
docker version                                      # the daemon must answer, or a Docker check cannot be measured
docker image inspect pi-agent:local                 # the image the Docker stories run
docker network ls | grep -w egress                  # the proxy test's premise (§2); `grep -x` never matches
docker rm -f hs_egress hs_key 2>/dev/null           # so tests/container.rs runs its real path
scripts/verify.sh                                   # baseline green, ~9–12 min (it prints its own counts)
```

If the baseline is red before you touch anything, that is the first thing you fix — and say so in Linear.

Then start §5 on **§1's issue** (`PER-84`, HS-052) and stop after step 11.

Before you begin, check that the last session left nothing behind — `subagent({action:"list"})` should show
no run in progress, `docker ps` none of its containers, and `git status --short` should look like the line
above. If it does not, that is the first thing you fix, and say so in Linear: a tree another session is still
editing is a stop-the-line error, not an inconvenience.

---

## 9. Reporting

The durable record is **Linear** (plan, demo output and ship note per issue), **`docs/review/PER-<n>.md`**
(the review verbatim and its `## Resolution` table) and **`git log`**. Keep a short ledger in the
conversation while the session runs (issue → plan → implement → review → fix → ship) so a restart can see it
from those three; do not carry old sessions' ledgers in this file.

If you stop before the issue ships, say so explicitly, name the next action, and leave the tree clean and the
gates green — **a half-finished story is worse than an honest stop**.

**The last shipped story** (`PER-83`, HS-051, ADR-0060; commits `39845a6`, `e566481`, `1892705`, `f652e2d`): the
drain, which is a **socket and a process flag and not a shutdown**. `holosmith drain [--wait 10m]` (the Commands
table's spelling; item 10's `holosmith admin drain` is the prose's older one, and there is no `admin` group for one
verb) speaks one request line `drain <millis>` to `<admin dir>/drain.sock` and prints the server's lines as they
arrive, and **the terminal word is the exit code** — `drained` is 0, and `expired` (a turn outlasted the wait, the
flag stays on, nothing is stopped) and `refused` are 1. `draining` is `AppState.drain`, an `Arc<AtomicBool>`, never a
row; the create, the prompt **and the resume** answer `503 draining` (the envelope's first non-4xx) while reads,
`abort`, `steer` and `delete` keep working. The wait is a 100 ms poll over `counts_by_status` for `running` only
(`starting` is reported once and left to the reconcile), and the stop is `Trigger::Drain` + `Stopper` with the reason
`server_restart` — the fifth policy that stops something, and the only one an operator asks for. The review raised one
P1 — the client's 5 s read timeout treated the server's *designed* silence between progress lines as a dead socket, so
`drain --wait 1m` failed after five seconds against any turn longer than that (the story's own demo) — and six
Standards findings, all fixed, none dismissed; the P1 was reproduced with a fake socket that stays silent for six
seconds and re-measured after the fix. On the way it also found and fixed a real defect: eight spawns in the tests
and gates were binding the machine's **real** `/run/holosmith/drain.sock`, and the leftover turned into an unrelated
`tests/deploy.rs` `perms` failure. The gate: **77 cases, 1101 assertions**, no `SKIP`; `cargo test` 919/51, `web` 52.
Its full record is Linear, `docs/review/PER-83.md` and `git log`; do not carry it forward.

**One thing this build does not do, recorded rather than discovered:** a resumed session's pi has **no
memory** of earlier turns (`--no-session`; the transcript is Holosmith's log). Replaying the log into a new pi
is a product decision with a token cost and needs its own story — ADR-0040 and ADR-0039 both say so, and
nothing in the backlog owns it yet.
